// rust-ime fcitx5 addon — an InputMethodEngine backed by the Rust core.
//
// Candidate UI: this addon forwards preedit + candidates over a Unix socket to
// the rust-ime Tauri app (our own gold UI). If the app is not running, it falls
// back to fcitx5's classicui candidate panel. Text is always committed through
// fcitx5's InputContext.

#include <fcitx/addonfactory.h>
#include <fcitx/addonmanager.h>
#include <fcitx/candidatelist.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputmethodengine.h>
#include <fcitx/inputpanel.h>
#include <fcitx/instance.h>
#include <fcitx/text.h>
#include <fcitx-utils/event.h>
#include <fcitx-utils/keysym.h>

#include <algorithm>
#include <cerrno>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>
#include <vector>

#include <fcntl.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

// ── Rust ABI (crates/ime-engine/src/ffi.rs) ──────────────────────────────
extern "C" {
struct RustIme;
RustIme *rust_ime_new();
void rust_ime_free(RustIme *h);
int rust_ime_key(RustIme *h, int kind, uint32_t cp);
int rust_ime_active(RustIme *h);
int rust_ime_preedit(RustIme *h, char *buf, int cap);
int rust_ime_candidate_count(RustIme *h);
int rust_ime_candidate(RustIme *h, int idx, char *buf, int cap);
int rust_ime_commit_candidate(RustIme *h, int idx);
int rust_ime_take_commit(RustIme *h, char *buf, int cap);
void rust_ime_reset(RustIme *h);
void rust_ime_configure(RustIme *h, int max_candidates, int prediction,
                        int learn, int fuzzy, int chinese_punctuation);
int rust_ime_selected(RustIme *h);
int rust_ime_move_selection(RustIme *h, int delta);
}

namespace {

constexpr int kKeyLetter = 0;
constexpr int kKeyDigit = 1;
constexpr int kKeyBackspace = 2;
constexpr int kKeyEnter = 3;
constexpr int kKeySpace = 4;
constexpr int kKeyEscape = 5;
constexpr int kKeyLeft = 6;
constexpr int kKeyRight = 7;
constexpr int kKeyPunct = 9;

/// ASCII punctuation keysyms → ASCII code point.
bool punctKeySym(fcitx::KeySym sym, uint32_t &cp) {
    switch (sym) {
    case FcitxKey_comma: cp = ','; return true;
    case FcitxKey_period: cp = '.'; return true;
    case FcitxKey_question: cp = '?'; return true;
    case FcitxKey_exclam: cp = '!'; return true;
    case FcitxKey_colon: cp = ':'; return true;
    case FcitxKey_semicolon: cp = ';'; return true;
    case FcitxKey_apostrophe: cp = '\''; return true;
    case FcitxKey_quotedbl: cp = '"'; return true;
    case FcitxKey_parenleft: cp = '('; return true;
    case FcitxKey_parenright: cp = ')'; return true;
    case FcitxKey_bracketleft: cp = '['; return true;
    case FcitxKey_bracketright: cp = ']'; return true;
    case FcitxKey_less: cp = '<'; return true;
    case FcitxKey_greater: cp = '>'; return true;
    case FcitxKey_backslash: cp = '\\'; return true;
    case FcitxKey_slash: cp = '/'; return true;
    case FcitxKey_at: cp = '@'; return true;
    case FcitxKey_numbersign: cp = '#'; return true;
    case FcitxKey_dollar: cp = '$'; return true;
    case FcitxKey_percent: cp = '%'; return true;
    case FcitxKey_asciicircum: cp = '^'; return true;
    case FcitxKey_ampersand: cp = '&'; return true;
    case FcitxKey_asterisk: cp = '*'; return true;
    case FcitxKey_minus: cp = '-'; return true;
    case FcitxKey_underscore: cp = '_'; return true;
    case FcitxKey_equal: cp = '='; return true;
    case FcitxKey_plus: cp = '+'; return true;
    case FcitxKey_asciitilde: cp = '~'; return true;
    case FcitxKey_grave: cp = '`'; return true;
    case FcitxKey_braceleft: cp = '{'; return true;
    case FcitxKey_braceright: cp = '}'; return true;
    case FcitxKey_bar: cp = '|'; return true;
    default: return false;
    }
}

std::string jsonEscape(const std::string &s) {
    std::string o;
    o.reserve(s.size() + 8);
    for (unsigned char c : s) {
        switch (c) {
        case '"':
        case '\\':
            o += '\\';
            o += static_cast<char>(c);
            break;
        case '\n':
            o += "\\n";
            break;
        case '\r':
            o += "\\r";
            break;
        default:
            o += static_cast<char>(c);
        }
    }
    return o;
}

class RustImeEngine : public fcitx::InputMethodEngineV2 {
public:
    explicit RustImeEngine(fcitx::Instance *instance) : instance_(instance) {}

    ~RustImeEngine() override {
        closeSocket();
        std::lock_guard<std::mutex> lock(mutex_);
        for (auto &[ic, handle] : engines_) {
            rust_ime_free(handle);
        }
        engines_.clear();
    }

    void activate(const fcitx::InputMethodEntry &,
                  fcitx::InputContextEvent &event) override {
        handleFor(event.inputContext());
    }

    void deactivate(const fcitx::InputMethodEntry &,
                    fcitx::InputContextEvent &event) override {
        auto *ic = event.inputContext();
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = engines_.find(ic);
        if (it != engines_.end()) {
            rust_ime_reset(it->second);
            rust_ime_free(it->second);
            engines_.erase(it);
        }
        ic->inputPanel().reset();
        ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
        lastIc_ = nullptr;
    }

    void reset(const fcitx::InputMethodEntry &,
               fcitx::InputContextEvent &event) override {
        auto *ic = event.inputContext();
        if (auto *h = handleFor(ic)) {
            rust_ime_reset(h);
        }
        ic->inputPanel().reset();
        ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
        lastIc_ = nullptr;
    }

    void keyEvent(const fcitx::InputMethodEntry &,
                  fcitx::KeyEvent &keyEvent) override {
        pollCommands();
        if (keyEvent.isRelease()) {
            return;
        }
        auto *ic = keyEvent.inputContext();
        auto *h = handleFor(ic);
        if (!h) {
            return;
        }

        // Leave every Ctrl/Alt/Super shortcut to the application.
        const auto states = keyEvent.key().states();
        if (states.test(fcitx::KeyState::Ctrl) ||
            states.test(fcitx::KeyState::Alt) ||
            states.test(fcitx::KeyState::Super)) {
            return;
        }

        const fcitx::KeySym sym = keyEvent.key().sym();
        int kind = -1;
        uint32_t cp = 0;

        if (sym >= FcitxKey_a && sym <= FcitxKey_z) {
            kind = kKeyLetter;
            cp = static_cast<uint32_t>(sym);
        } else if (sym >= FcitxKey_1 && sym <= FcitxKey_9) {
            kind = kKeyDigit;
            cp = static_cast<uint32_t>(sym - FcitxKey_0);
        } else if (sym == FcitxKey_BackSpace) {
            kind = kKeyBackspace;
        } else if (sym == FcitxKey_Return) {
            kind = kKeyEnter;
        } else if (sym == FcitxKey_space) {
            kind = kKeySpace;
        } else if (sym == FcitxKey_Escape) {
            kind = kKeyEscape;
        } else if (sym == FcitxKey_Left) {
            kind = kKeyLeft;
        } else if (sym == FcitxKey_Right) {
            kind = kKeyRight;
        } else if (punctKeySym(sym, cp)) {
            kind = kKeyPunct;
        }

        if (kind < 0) {
            return;
        }

        int handled = rust_ime_key(h, kind, cp);
        if (handled) {
            flushCommits(ic);
            updateUI(ic);
            keyEvent.filterAndAccept();
        }
        pollCommands();
    }

private:
    RustIme *handleFor(fcitx::InputContext *ic) {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = engines_.find(ic);
        if (it != engines_.end()) {
            return it->second;
        }
        RustIme *h = rust_ime_new();
        applyTo(h);
        engines_.emplace(ic, h);
        return h;
    }

    /// Push the current settings into one engine handle.
    void applyTo(RustIme *h) {
        rust_ime_configure(h, maxCandidates_, prediction_, learn_, fuzzy_,
                           punct_);
    }

    /// Settings pushed by the app (candidate count, prediction, learn,
    /// fuzzy pinyin, Chinese punctuation).
    void applySettings(int candidates, int prediction, int learn, int fuzzy,
                       int punct) {
        maxCandidates_ = std::clamp(candidates, 1, 10);
        prediction_ = prediction != 0;
        learn_ = learn != 0;
        fuzzy_ = fuzzy != 0;
        punct_ = punct != 0;
        std::lock_guard<std::mutex> lock(mutex_);
        for (auto &[ic, h] : engines_) {
            (void)ic;
            applyTo(h);
        }
    }

    void moveSelection(int delta) {
        if (!lastIc_ || delta == 0) {
            return;
        }
        auto *h = handleFor(lastIc_);
        if (rust_ime_move_selection(h, delta)) {
            updateUI(lastIc_);
        }
    }

    void flushCommits(fcitx::InputContext *ic) {
        auto *h = handleFor(ic);
        char buf[1024];
        int n;
        while ((n = rust_ime_take_commit(h, buf, sizeof(buf))) > 0) {
            ic->commitString(std::string(buf, static_cast<size_t>(n)));
        }
    }

    // ── IPC with the rust-ime Tauri app ────────────────────────────────
    void closeSocket() {
        // Destroy the watcher before the fd it references.
        ioEvent_.reset();
        if (sock_ >= 0) {
            ::close(sock_);
            sock_ = -1;
        }
    }

    bool ensureSocket() {
        if (sock_ >= 0) {
            return true;
        }
        const char *rt = std::getenv("XDG_RUNTIME_DIR");
        std::string path = std::string(rt ? rt : "/tmp") + "/rust-ime/ui.sock";
        int fd = ::socket(AF_UNIX, SOCK_STREAM, 0);
        if (fd < 0) {
            return false;
        }
        sockaddr_un addr{};
        addr.sun_family = AF_UNIX;
        std::strncpy(addr.sun_path, path.c_str(), sizeof(addr.sun_path) - 1);
        if (::connect(fd, reinterpret_cast<sockaddr *>(&addr), sizeof(addr)) != 0) {
            ::close(fd);
            return false;
        }
        int flags = ::fcntl(fd, F_GETFL, 0);
        ::fcntl(fd, F_SETFL, flags | O_NONBLOCK);
        sock_ = fd;
        // Selection commands from the UI arrive without a following key
        // event, so watch the socket instead of polling it on key presses.
        ioEvent_ = instance_->eventLoop().addIOEvent(
            fd, fcitx::IOEventFlag::In,
            [this](fcitx::EventSourceIO *, int, fcitx::IOEventFlags) {
                pollCommands();
                return true;
            });
        return true;
    }

    void sendLine(const std::string &line) {
        if (!ensureSocket()) {
            return;
        }
        ssize_t n = ::write(sock_, line.data(), line.size());
        if (n < 0 && errno != EAGAIN) {
            closeSocket();
        }
    }

    /// Read pending "S<idx>\n" selection commands from the app.
    void pollCommands() {
        if (sock_ < 0) {
            return;
        }
        char buf[256];
        ssize_t n = ::read(sock_, buf, sizeof(buf));
        if (n == 0 || (n < 0 && errno != EAGAIN)) {
            closeSocket();
            return;
        }
        if (n < 0) {
            return;
        }
        std::string s(buf, static_cast<size_t>(n));
        size_t pos = 0;
        while (pos < s.size()) {
            size_t nl = s.find('\n', pos);
            if (nl == std::string::npos) {
                break;
            }
            std::string line = s.substr(pos, nl - pos);
            pos = nl + 1;
            if (line.size() >= 2 && line[0] == 'S') {
                int idx = std::atoi(line.c_str() + 1);
                selectCandidate(idx);
            } else if (line.size() >= 2 && line[0] == 'M') {
                moveSelection(std::atoi(line.c_str() + 1));
            } else if (line.size() >= 2 && line[0] == 'G') {
                int c = 10, p = 1, l = 1, f = 0, z = 1;
                if (std::sscanf(line.c_str() + 1, "%d,%d,%d,%d,%d", &c, &p,
                                &l, &f, &z) == 5) {
                    applySettings(c, p, l, f, z);
                }
            }
        }
    }

    void selectCandidate(int idx) {
        if (!lastIc_ || idx < 0 ||
            idx >= static_cast<int>(lastCands_.size())) {
            return;
        }
        auto *h = handleFor(lastIc_);
        rust_ime_commit_candidate(h, idx);
        flushCommits(lastIc_);
        updateUI(lastIc_);
    }

    void updateUI(fcitx::InputContext *ic) {
        auto *h = handleFor(ic);
        char buf[4096];

        int n = rust_ime_preedit(h, buf, sizeof(buf));
        std::string preedit = n > 0 ? std::string(buf, static_cast<size_t>(n))
                                    : std::string();

        int count = rust_ime_candidate_count(h);
        std::vector<std::string> cands;
        cands.reserve(static_cast<size_t>(count));
        for (int i = 0; i < count; ++i) {
            char cbuf[512];
            int cn = rust_ime_candidate(h, i, cbuf, sizeof(cbuf));
            if (cn > 0) {
                cands.emplace_back(cbuf, static_cast<size_t>(cn));
            }
        }

        // Inline preedit in the client (underlined pinyin) stays with fcitx5.
        if (!preedit.empty()) {
            fcitx::Text text;
            text.append(preedit);
            text.setCursor(text.toString().size());
            ic->inputPanel().setClientPreedit(text);
        } else {
            ic->inputPanel().setClientPreedit(fcitx::Text());
        }

        if (ensureSocket()) {
            // Our Tauri window renders the candidates.
            const auto &r = ic->cursorRect();
            std::string line = "{\"preedit\":\"" + jsonEscape(preedit) +
                               "\",\"candidates\":[";
            for (size_t i = 0; i < cands.size(); ++i) {
                if (i) {
                    line += ',';
                }
                line += '"' + jsonEscape(cands[i]) + '"';
            }
            line += "],\"selected\":" + std::to_string(rust_ime_selected(h)) +
                    ",\"x\":" + std::to_string(r.left()) +
                    ",\"y\":" + std::to_string(r.top()) +
                    ",\"w\":" + std::to_string(r.width()) +
                    ",\"h\":" + std::to_string(r.height()) + "}\n";
            sendLine(line);
            lastIc_ = ic;
            lastCands_ = cands;
            // Keep classicui from drawing a second candidate panel.
            ic->inputPanel().setCandidateList(nullptr);
        } else {
            // Fallback: fcitx5's own candidate panel.
            if (count > 0) {
                auto list = std::make_unique<fcitx::CommonCandidateList>();
                list->setPageSize(10);
                for (const auto &c : cands) {
                    list->append<fcitx::DisplayOnlyCandidateWord>(
                        fcitx::Text(c));
                }
                ic->inputPanel().setCandidateList(std::move(list));
            } else {
                ic->inputPanel().setCandidateList(nullptr);
            }
        }

        ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
    }

    fcitx::Instance *instance_;
    std::mutex mutex_;
    std::unordered_map<fcitx::InputContext *, RustIme *> engines_;
    int sock_ = -1;
    std::unique_ptr<fcitx::EventSourceIO> ioEvent_;
    fcitx::InputContext *lastIc_ = nullptr;
    std::vector<std::string> lastCands_;
    int maxCandidates_ = 10;
    bool prediction_ = true;
    bool learn_ = true;
    bool fuzzy_ = false;
    bool punct_ = true;
};

class RustImeEngineFactory : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override {
        return new RustImeEngine(manager->instance());
    }
};

} // namespace

FCITX_ADDON_FACTORY_V2(rustime, RustImeEngineFactory);
