#include "desktop_ime.h"

#include <flutter/standard_method_codec.h>
#include <imm.h>

DesktopIme::DesktopIme(flutter::FlutterEngine* engine, HWND view) : view_(view) {
  channel_ = std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
      engine->messenger(), "pb_mapper/ime",
      &flutter::StandardMethodCodec::GetInstance());
  channel_->SetMethodCallHandler([this](const auto& call, auto result) {
    if (call.method_name() != "setEnabled") {
      result->NotImplemented();
      return;
    }
    const auto* enabled = call.arguments() ? std::get_if<bool>(call.arguments()) : nullptr;
    if (!enabled) {
      result->Error("pb_mapper_ime", "setEnabled takes a bool");
      return;
    }
    SetEnabled(*enabled);
    result->Success();
  });
}

DesktopIme::~DesktopIme() {
  channel_->SetMethodCallHandler(nullptr);
  SetEnabled(true);
}

// IACE_DEFAULT gives the window its default input context back; 0 with no
// context leaves it none, so no input method sees its keys.
void DesktopIme::SetEnabled(bool enabled) {
  if (enabled == enabled_) return;
  if (ImmAssociateContextEx(view_, nullptr, enabled ? IACE_DEFAULT : 0)) {
    enabled_ = enabled;
  }
}
