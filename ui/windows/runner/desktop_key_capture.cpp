#include "desktop_key_capture.h"

#include <flutter/method_result_functions.h>
#include <flutter/standard_method_codec.h>

#include <array>
#include <cstdint>
#include <string>
#include <utility>
#include <variant>

namespace {

// A key the hook took, posted to the window: `wparam` is its virtual key with
// kDown and kRepeat, `lparam` its scan code (ScanCode).
constexpr UINT kCapturedKey = WM_APP + 0x414;
constexpr WPARAM kDown = WPARAM{1} << 16;
constexpr WPARAM kRepeat = WPARAM{1} << 17;

// The Left Ctrl a layout with AltGr sends ahead of Right Alt: no key of its
// own.
constexpr DWORD kAltGrControl = 0x21D;

// The scan code as Flutter's Windows tables name a key, and as its embedder
// names the keys it gets: 0xE0 ahead of an extended one. It names the key's
// place, which a key's up shares with its down.
LPARAM ScanCode(const KBDLLHOOKSTRUCT& event) {
  // Under Alt the keyboard reports Print Screen as SysRq (0x54), which the
  // tables do not name.
  if (event.vkCode == VK_SNAPSHOT) return 0xE037;
  const DWORD scan = event.scanCode & 0xFF;
  if (scan == 0) {
    return static_cast<LPARAM>(
        MapVirtualKeyW(event.vkCode, MAPVK_VK_TO_VSC_EX));
  }
  // Right Shift has a scan code of its own and is never extended.
  if (event.vkCode == VK_RSHIFT) return static_cast<LPARAM>(scan);
  return static_cast<LPARAM>((event.flags & LLKHF_EXTENDED) ? 0xE000 | scan
                                                            : scan);
}

// A key's place in the hook's table: its scan code (ScanCode), the prefix as
// a bit of its own.
size_t Place(LPARAM scan) {
  const auto prefix = (scan >> 8) & 0xFF;
  return static_cast<size_t>(scan & 0xFF) |
         (prefix == 0xE0   ? size_t{0x100}
          : prefix == 0xE1 ? size_t{0x200}
                           : size_t{0});
}

// The hook's state, on the hook's thread: what it did with the key at each
// place, from its down to its up. A key taken goes down and up in Dart only;
// a key passed, down and up in the system only. By place, not key code: two
// keys share a code (Enter and the keypad's, Home and the keypad's 7 with Num
// Lock off), and Num Lock with Shift, or a layout switch, can change a key's
// code between its down and its up.
class Hook {
 public:
  Hook(HWND window, const std::atomic<bool>* composing)
      : window_(window), composing_(composing) {
    PassHeldKeys();
  }

  // True: the system must not see this event.
  bool Take(WPARAM message, const KBDLLHOOKSTRUCT& event) {
    const DWORD vk = event.vkCode;
    // VK_PACKET is text a program types, not a key.
    if (vk == 0 || vk > 0xFE || vk == VK_PACKET) return false;
    const bool down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
    if (vk == VK_LCONTROL && event.scanCode == kAltGrControl) {
      altgr_ = down;
      // With AltGr held when capture started, Left Ctrl was marked down:
      // this is its up.
      Key& left = keys_[Place(0x1D)];
      if (!down && left.held == Held::kPassed) left = Key{};
      return false;
    }
    const LPARAM scan = ScanCode(event);
    Key& key = keys_[Place(scan)];
    if (!down) {
      const bool taken = key.held == Held::kTaken;
      if (taken) Post(key, 0);
      key = Key{};
      return taken;
    }
    if (key.held == Held::kTaken) {
      Post(key, kDown | kRepeat);
      return true;
    }
    if (key.held == Held::kPassed) return false;
    const bool take = GetForegroundWindow() == window_ && Takes(vk);
    key = Key{take ? Held::kTaken : Held::kPassed, vk, scan};
    if (take) Post(key, kDown);
    return take;
  }

  // Capture stops: each key still taken goes up in Dart. The system never
  // saw it go down, so its real up may go there.
  void ReleaseTaken() {
    for (Key& key : keys_) {
      if (key.held != Held::kTaken) continue;
      Post(key, 0);
      key = Key{};
    }
  }

  // The secure desktop (Win+L, Ctrl+Alt+Del, a UAC prompt) took the keyboard
  // or gave it back, and no key's up came here meanwhile: the keys taken go
  // up, and what is down now is the system's.
  void DesktopSwitched() {
    ReleaseTaken();
    keys_.fill(Key{});
    altgr_ = false;
    PassHeldKeys();
  }

 private:
  enum class Held : uint8_t { kNone, kPassed, kTaken };
  struct Key {
    Held held = Held::kNone;
    DWORD vk = 0;
    LPARAM scan = 0;
  };

  // Keys down now went to the system, and so do their ups. Off the active
  // desktop none reads as down.
  void PassHeldKeys() {
    for (int vk = 1; vk < 0xFF; ++vk) {
      // Windows reports the sided codes too; a hook gets only those.
      if (vk == VK_SHIFT || vk == VK_CONTROL || vk == VK_MENU) continue;
      if (!(GetAsyncKeyState(vk) & 0x8000)) continue;
      const UINT scan =
          MapVirtualKeyW(static_cast<UINT>(vk), MAPVK_VK_TO_VSC_EX);
      if (scan == 0) continue;
      Key& key = keys_[Place(static_cast<LPARAM>(scan))];
      if (key.held == Held::kNone) {
        key = Key{Held::kPassed, static_cast<DWORD>(vk),
                  static_cast<LPARAM>(scan)};
      }
    }
  }

  // This computer's shortcuts: see the header.
  bool Takes(DWORD vk) const {
    switch (vk) {
      case VK_LWIN:
      case VK_RWIN:
      case VK_LMENU:
      case VK_SNAPSHOT:
        return true;
      case VK_RMENU:
        return !altgr_;
      case VK_F10:
        // A Japanese input method's half-width letters while it composes.
        return Holding() || !composing_->load(std::memory_order_relaxed);
      case VK_ESCAPE:
        return Down(VK_LCONTROL) || Down(VK_RCONTROL) || Holding();
      case VK_CAPITAL:
      case VK_NUMLOCK:
      case VK_SCROLL:
        // This keyboard's lights and layout keep them.
        return false;
      default:
        return Holding();
    }
  }

  // The Windows key or Alt was taken and is still down.
  bool Holding() const {
    return Taken(VK_LWIN) || Taken(VK_RWIN) || Taken(VK_LMENU) ||
           Taken(VK_RMENU);
  }

  // A key with code `vk` is down, passed or taken.
  bool Down(DWORD vk) const {
    for (const Key& key : keys_) {
      if (key.held != Held::kNone && key.vk == vk) return true;
    }
    return false;
  }

  // A key with code `vk` was taken and is still down.
  bool Taken(DWORD vk) const {
    for (const Key& key : keys_) {
      if (key.held == Held::kTaken && key.vk == vk) return true;
    }
    return false;
  }

  // Under the code and place it went down with.
  void Post(const Key& key, WPARAM flags) const {
    PostMessageW(window_, kCapturedKey, static_cast<WPARAM>(key.vk) | flags,
                 key.scan);
  }

  HWND window_;
  const std::atomic<bool>* composing_;
  std::array<Key, 0x300> keys_{};
  bool altgr_ = false;
};

thread_local Hook* t_hook = nullptr;

LRESULT CALLBACK KeyboardHook(int code, WPARAM wparam, LPARAM lparam) {
  if (code == HC_ACTION && t_hook != nullptr &&
      t_hook->Take(wparam,
                   *reinterpret_cast<const KBDLLHOOKSTRUCT*>(lparam))) {
    return 1;
  }
  return CallNextHookEx(nullptr, code, wparam, lparam);
}

// EVENT_SYSTEM_DESKTOPSWITCH, on the hook's thread.
void CALLBACK OnDesktopSwitch(HWINEVENTHOOK, DWORD, HWND, LONG, LONG, DWORD,
                              DWORD) {
  if (t_hook != nullptr) t_hook->DesktopSwitched();
}

// The hook's own thread. The system calls a low-level hook from the message
// loop of the thread that set it, waits for it with this computer's whole
// keyboard, and removes one that keeps it waiting: the window's thread, busy
// with the picture and Flutter, must not be that thread, and the picture's
// decoding must not starve this one. `*id` is this thread's id once the hook
// is set, 0 if it could not be; `started` is set after either, and neither is
// touched again.
void RunHook(HWND window, const std::atomic<bool>* composing, HANDLE started,
             DWORD* id) {
  SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
  MSG message;
  // The queue exists before the thread's id goes out for WM_QUIT.
  PeekMessageW(&message, nullptr, WM_USER, WM_USER, PM_NOREMOVE);
  Hook hook(window, composing);
  t_hook = &hook;
  HHOOK handle = SetWindowsHookExW(WH_KEYBOARD_LL, KeyboardHook,
                                   GetModuleHandleW(nullptr), 0);
  HWINEVENTHOOK desktops =
      handle == nullptr
          ? nullptr
          : SetWinEventHook(EVENT_SYSTEM_DESKTOPSWITCH,
                            EVENT_SYSTEM_DESKTOPSWITCH, nullptr,
                            OnDesktopSwitch, 0, 0, WINEVENT_OUTOFCONTEXT);
  *id = handle == nullptr ? 0 : GetCurrentThreadId();
  SetEvent(started);
  if (handle == nullptr) {
    t_hook = nullptr;
    return;
  }
  while (GetMessageW(&message, nullptr, 0, 0) > 0) {
  }
  if (desktops != nullptr) UnhookWinEvent(desktops);
  UnhookWindowsHookEx(handle);
  t_hook = nullptr;
  hook.ReleaseTaken();
}

}  // namespace

DesktopKeyCapture::DesktopKeyCapture(flutter::FlutterEngine* engine,
                                     HWND window)
    : window_(window) {
  channel_ = std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
      engine->messenger(), "pb_mapper/key_capture",
      &flutter::StandardMethodCodec::GetInstance());
  channel_->SetMethodCallHandler([this](const auto& call, auto result) {
    const std::string& method = call.method_name();
    if (method != "setCaptured" && method != "setComposing") {
      result->NotImplemented();
      return;
    }
    const auto* value =
        call.arguments() ? std::get_if<bool>(call.arguments()) : nullptr;
    if (!value) {
      result->Error("pb_mapper_key_capture", method + " takes a bool");
      return;
    }
    if (method == "setComposing") {
      composing_.store(*value, std::memory_order_relaxed);
    } else if (!SetCaptured(*value)) {
      result->Error("pb_mapper_key_capture",
                    "the keyboard hook could not be set");
      return;
    }
    result->Success();
  });
}

DesktopKeyCapture::~DesktopKeyCapture() {
  channel_->SetMethodCallHandler(nullptr);
  SetCaptured(false);
}

bool DesktopKeyCapture::SetCaptured(bool captured) {
  if (captured == thread_.joinable()) return true;
  if (!captured) {
    PostThreadMessageW(thread_id_, WM_QUIT, 0, 0);
    thread_.join();
    thread_id_ = 0;
    return true;
  }
  HANDLE started = CreateEventW(nullptr, TRUE, FALSE, nullptr);
  if (started == nullptr) return false;
  DWORD id = 0;
  thread_ = std::thread(RunHook, window_, &composing_, started, &id);
  WaitForSingleObject(started, INFINITE);
  CloseHandle(started);
  thread_id_ = id;
  if (thread_id_ == 0) {
    thread_.join();
    return false;
  }
  return true;
}

bool DesktopKeyCapture::HandleMessage(UINT message, WPARAM wparam,
                                      LPARAM lparam) {
  if (message != kCapturedKey) return false;
  flutter::EncodableMap key{
      {flutter::EncodableValue("vk"),
       flutter::EncodableValue(static_cast<int32_t>(wparam & 0xFF))},
      {flutter::EncodableValue("scan"),
       flutter::EncodableValue(static_cast<int32_t>(lparam))},
      {flutter::EncodableValue("down"),
       flutter::EncodableValue((wparam & kDown) != 0)},
      {flutter::EncodableValue("repeat"),
       flutter::EncodableValue((wparam & kRepeat) != 0)},
  };
  // No Dart handler: nothing takes the keys, so the system has them back.
  std::weak_ptr<DesktopKeyCapture*> self = self_;
  channel_->InvokeMethod(
      "key", std::make_unique<flutter::EncodableValue>(std::move(key)),
      std::make_unique<flutter::MethodResultFunctions<>>(
          nullptr, nullptr, [self]() {
            if (auto capture = self.lock()) (*capture)->SetCaptured(false);
          }));
  return true;
}
