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
#include <fcitx-utils/keysym.h>

#include <cerrno>
#include <cstdint>
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
        if (sock_ >= 0) {
            ::close(sock_);
        }
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
        engines_.emplace(ic, h);
        return h;
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
        return true;
    }

    void sendLine(const std::string &line) {
        if (!ensureSocket()) {
            return;
        }
        ssize_t n = ::write(sock_, line.data(), line.size());
        if (n < 0 && errno != EAGAIN) {
            ::close(sock_);
            sock_ = -1;
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
            ::close(sock_);
            sock_ = -1;
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
            line += "],\"x\":" + std::to_string(r.left()) +
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
    fcitx::InputContext *lastIc_ = nullptr;
    std::vector<std::string> lastCands_;
};

class RustImeEngineFactory : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override {
        return new RustImeEngine(manager->instance());
    }
};

} // namespace

FCITX_ADDON_FACTORY_V2(rustime, RustImeEngineFactory);
