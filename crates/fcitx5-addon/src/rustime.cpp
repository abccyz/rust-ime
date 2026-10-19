// rust-ime fcitx5 addon — an InputMethodEngine backed by the Rust core.
//
// The heavy lifting (pinyin segmentation, candidate generation, learning) is
// in `crates/ime-engine`, exposed over a small C ABI (see ffi.rs). This shim
// only translates fcitx5 KeyEvents into that ABI and pushes the resulting
// preedit / candidate list back into fcitx5's InputPanel, so fcitx5's own
// candidate window does the rendering.

#include <fcitx/addonfactory.h>
#include <fcitx/addonmanager.h>
#include <fcitx/candidatelist.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputmethodengine.h>
#include <fcitx/inputpanel.h>
#include <fcitx/instance.h>
#include <fcitx/text.h>
#include <fcitx-utils/keysym.h>

#include <cstdint>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>

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

class RustImeEngine : public fcitx::InputMethodEngineV2 {
public:
    explicit RustImeEngine(fcitx::Instance *instance) : instance_(instance) {}

    ~RustImeEngine() override {
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
    }

    void reset(const fcitx::InputMethodEntry &,
               fcitx::InputContextEvent &event) override {
        auto *ic = event.inputContext();
        if (auto *h = handleFor(ic)) {
            rust_ime_reset(h);
        }
        ic->inputPanel().reset();
        ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
    }

    void keyEvent(const fcitx::InputMethodEntry &,
                  fcitx::KeyEvent &keyEvent) override {
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

    void updateUI(fcitx::InputContext *ic) {
        auto *h = handleFor(ic);
        char buf[4096];

        int n = rust_ime_preedit(h, buf, sizeof(buf));
        if (n > 0) {
            fcitx::Text preedit;
            preedit.append(std::string(buf, static_cast<size_t>(n)));
            preedit.setCursor(preedit.toString().size());
            ic->inputPanel().setClientPreedit(preedit);
        } else {
            ic->inputPanel().setClientPreedit(fcitx::Text());
        }

        int count = rust_ime_candidate_count(h);
        if (count > 0) {
            auto list = std::make_unique<fcitx::CommonCandidateList>();
            list->setPageSize(9);
            for (int i = 0; i < count; ++i) {
                char cbuf[512];
                int cn = rust_ime_candidate(h, i, cbuf, sizeof(cbuf));
                if (cn > 0) {
                    list->append<fcitx::Text>(
                        std::string(cbuf, static_cast<size_t>(cn)));
                }
            }
            ic->inputPanel().setCandidateList(std::move(list));
        } else {
            ic->inputPanel().setCandidateList(nullptr);
        }

        ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
    }

    fcitx::Instance *instance_;
    std::mutex mutex_;
    std::unordered_map<fcitx::InputContext *, RustIme *> engines_;
};

class RustImeEngineFactory : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override {
        return new RustImeEngine(manager->instance());
    }
};

} // namespace

FCITX_ADDON_FACTORY_V2(rustime, RustImeEngineFactory);
