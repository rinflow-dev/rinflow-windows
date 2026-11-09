// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The 设置 page. Values come from the snapshot's `settings` (see
// agent/src/settings.rs); every edit goes back as `update_settings`.

#include "pch.h"
#include "MainWindow.xaml.h"
#include "Strings.h"
#include "AppShell.h"

// Fully qualified: WIL declares a global ::Microsoft namespace as well.
using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;
using namespace winrt::Microsoft::UI::Xaml::Controls;
using namespace winrt::Microsoft::UI::Xaml::Input;
using namespace winrt::Windows::Data::Json;

namespace
{
    // RegisterHotKey modifier bits, as the agent stores them.
    constexpr uint32_t ModAlt = 0x1;
    constexpr uint32_t ModControl = 0x2;
    constexpr uint32_t ModShift = 0x4;
    constexpr uint32_t ModWin = 0x8;
    constexpr uint32_t DefaultModifiers = ModControl | ModAlt;
    constexpr uint32_t DefaultKey = 'L';

    bool IsModifierKey(uint32_t vk)
    {
        switch (vk)
        {
        case VK_SHIFT: case VK_CONTROL: case VK_MENU: case VK_LWIN: case VK_RWIN:
        case VK_LSHIFT: case VK_RSHIFT: case VK_LCONTROL: case VK_RCONTROL: case VK_LMENU: case VK_RMENU:
        case VK_CAPITAL: case VK_NUMLOCK:
            return true;
        default:
            return false;
        }
    }

    hstring KeyName(uint32_t vk)
    {
        if ((vk >= 'A' && vk <= 'Z') || (vk >= '0' && vk <= '9'))
        {
            return hstring(std::wstring(1, static_cast<wchar_t>(vk)));
        }
        if (vk >= VK_F1 && vk <= VK_F24)
        {
            return hstring(std::format(L"F{}", vk - VK_F1 + 1));
        }
        UINT scan = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC);
        switch (vk)
        {
        case VK_LEFT: case VK_RIGHT: case VK_UP: case VK_DOWN: case VK_PRIOR: case VK_NEXT:
        case VK_END: case VK_HOME: case VK_INSERT: case VK_DELETE: case VK_DIVIDE:
            scan |= 0x100; // extended key
            break;
        }
        wchar_t name[64]{};
        if (scan != 0 && GetKeyNameTextW(static_cast<LONG>(scan << 16), name, 64) > 0)
        {
            return name;
        }
        return hstring(std::format(L"0x{:02X}", vk));
    }

    std::vector<hstring> HotkeyParts(uint32_t modifiers, uint32_t key)
    {
        std::vector<hstring> parts;
        if (modifiers & ModControl) parts.push_back(L"Ctrl");
        if (modifiers & ModAlt) parts.push_back(L"Alt");
        if (modifiers & ModShift) parts.push_back(L"Shift");
        if (modifiers & ModWin) parts.push_back(L"Win");
        if (key != 0) parts.push_back(KeyName(key));
        return parts;
    }

    bool IsValidHotkey(uint32_t modifiers, uint32_t key)
    {
        return (modifiers & (ModControl | ModAlt | ModWin)) != 0 && key != 0 && !IsModifierKey(key);
    }

    void SetSwitch(ToggleSwitch const& toggle, TextBlock const& state, bool on)
    {
        if (toggle.IsOn() != on)
        {
            toggle.IsOn(on);
        }
        state.Text(RinflowApp::Text(on ? L"On" : L"Off"));
    }

    // "3fa27c…" (64 hex digits) as "3F:A2:7C:…", the way certificate
    // fingerprints are usually shown.
    std::wstring ColonHex(std::wstring_view hex)
    {
        std::wstring text;
        for (size_t i = 0; i < hex.size(); ++i)
        {
            if (i > 0 && i % 2 == 0) text.push_back(L':');
            text.push_back(static_cast<wchar_t>(towupper(hex[i])));
        }
        return text;
    }

    JsonObject HotkeyValue(uint32_t modifiers, uint32_t key)
    {
        JsonObject value;
        value.Insert(L"modifiers", JsonValue::CreateNumberValue(modifiers));
        value.Insert(L"key", JsonValue::CreateNumberValue(key));
        return value;
    }

    // What one of a Mac's modifier keys can play as here, in the order the
    // custom choices list them (the agent's `modifier_map`, keys by position:
    // "super" is ⌘ on a Mac and the Windows key here).
    constexpr std::wstring_view ModifierTargets[] = { L"control", L"super", L"alt", L"shift", L"none" };

    hstring ModifierTargetLabel(std::wstring_view target)
    {
        if (target == L"control") return L"Ctrl";
        if (target == L"super") return L"Win";
        if (target == L"alt") return L"Alt";
        if (target == L"shift") return L"Shift";
        return RinflowApp::Text(L"ModifierNone");
    }

    int ModifierTargetIndex(std::wstring_view target)
    {
        for (int i = 0; i < static_cast<int>(std::size(ModifierTargets)); ++i)
        {
            if (ModifierTargets[i] == target) return i;
        }
        return -1;
    }

    JsonObject ModifierMapValue(std::wstring_view super, std::wstring_view control, std::wstring_view alt)
    {
        JsonObject map;
        map.Insert(L"super", JsonValue::CreateStringValue(super));
        map.Insert(L"control", JsonValue::CreateStringValue(control));
        map.Insert(L"alt", JsonValue::CreateStringValue(alt));
        return map;
    }
}

namespace winrt::Rinflow::implementation
{
    void MainWindow::SetupSettingsPage()
    {
        auto weak = get_weak();
        // A switch that maps to one boolean setting.
        auto wire = [weak](ToggleSwitch const& toggle, TextBlock const& state, std::wstring key)
        {
            toggle.Toggled([weak, toggle, state, key](auto&&, auto&&)
            {
                auto self = weak.get();
                if (!self) return;
                state.Text(RinflowApp::Text(toggle.IsOn() ? L"On" : L"Off"));
                if (!self->m_applying)
                {
                    self->m_agent->UpdateSetting(key, JsonValue::CreateBooleanValue(toggle.IsOn()));
                }
            });
        };
        wire(LaunchToggle(), LaunchState(), L"launch_at_login");
        wire(TrayToggle(), TrayState(), L"keep_in_tray");
        wire(CornerToggle(), CornerState(), L"corner_guard");
        wire(FullscreenToggle(), FullscreenState(), L"pause_in_fullscreen");
        wire(ClipboardToggle(), ClipboardState(), L"clipboard");
        wire(PairedOnlyToggle(), PairedOnlyState(), L"paired_only");
        wire(SwitchHotkeysToggle(), SwitchHotkeysState(), L"switch_hotkeys");
        wire(KeepAwakeToggle(), KeepAwakeState(), L"keep_awake");
        wire(HideCursorToggle(), HideCursorState(), L"hide_cursor_on_leave");
        wire(LockSyncToggle(), LockSyncState(), L"lock_sync");

        // The agent's log folder (agent/src/log.rs).
        OpenLogsButton().Click([](auto&&, auto&&)
        {
            auto folder = wil::TryGetEnvironmentVariableW(L"LOCALAPPDATA");
            if (!folder) return;
            std::wstring path = std::wstring(folder.get()) + L"\\Rinflow\\logs";
            CreateDirectoryW(path.c_str(), nullptr);
            ShellExecuteW(nullptr, L"open", path.c_str(), nullptr, nullptr, SW_SHOWNORMAL);
        });

        // A Mac's modifier keys: a preset sends its map at once; 自定义 shows
        // one choice per key, each sending the whole map when changed.
        auto sendCustomModifiers = [weak]
        {
            auto self = weak.get();
            if (!self || self->m_applying) return;
            auto target = [](ComboBox const& combo)
            {
                auto index = combo.SelectedIndex();
                return index >= 0 && index < static_cast<int>(std::size(ModifierTargets))
                    ? ModifierTargets[index] : std::wstring_view{};
            };
            auto super = target(self->ModifierSuperCombo());
            auto control = target(self->ModifierControlCombo());
            auto alt = target(self->ModifierAltCombo());
            if (super.empty() || control.empty() || alt.empty()) return;
            self->m_agent->UpdateSetting(L"modifier_map", ModifierMapValue(super, control, alt));
        };
        for (auto const& combo : { ModifierSuperCombo(), ModifierControlCombo(), ModifierAltCombo() })
        {
            for (auto target : ModifierTargets)
            {
                ComboBoxItem item;
                item.Content(box_value(ModifierTargetLabel(target)));
                combo.Items().Append(item);
            }
            combo.SelectionChanged([sendCustomModifiers](auto&&, auto&&) { sendCustomModifiers(); });
        }
        ModifierPresetCombo().SelectionChanged([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self || self->m_applying) return;
            auto item = self->ModifierPresetCombo().SelectedItem().try_as<ComboBoxItem>();
            if (!item) return;
            auto preset = unbox_value_or<hstring>(item.Tag(), L"mac");
            if (preset == L"custom")
            {
                // The choices start from the map in use; nothing changes yet.
                self->m_modifierCustom = true;
                self->ModifierCustomPanel().Visibility(Visibility::Visible);
                self->ModifierMapDescription().Text(RinflowApp::Text(L"ModifierCustomDescription"));
                return;
            }
            self->m_modifierCustom = false;
            self->m_agent->UpdateSetting(L"modifier_map", preset == L"keep"
                ? ModifierMapValue(L"super", L"control", L"alt")
                : ModifierMapValue(L"control", L"super", L"alt"));
        });

        SwitchModifiersCombo().SelectionChanged([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self || self->m_applying) return;
            if (auto item = self->SwitchModifiersCombo().SelectedItem().try_as<ComboBoxItem>())
            {
                auto modifiers = std::stod(std::wstring(unbox_value_or<hstring>(item.Tag(), L"7")));
                self->m_agent->UpdateSetting(L"switch_modifiers", JsonValue::CreateNumberValue(modifiers));
            }
        });

        // Every string is loaded again in the new language: the app starts
        // afresh on this page (the agent applies it at once).
        LanguageCombo().SelectionChanged([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self || self->m_applying) return;
            auto item = self->LanguageCombo().SelectedItem().try_as<ComboBoxItem>();
            if (!item) return;
            auto language = unbox_value_or<hstring>(item.Tag(), L"system");
            self->m_agent->UpdateSetting(L"language", JsonValue::CreateStringValue(language));
            // Out of this event: the window closes. The new app is told the
            // language, as the agent may not have saved it yet.
            self->DispatcherQueue().TryEnqueue([language]
            {
                RinflowApp::RelaunchApp(language);
            });
        });

        ThemeCombo().SelectionChanged([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self || self->m_applying) return;
            if (auto item = self->ThemeCombo().SelectedItem().try_as<ComboBoxItem>())
            {
                auto theme = unbox_value_or<hstring>(item.Tag(), L"system");
                self->ApplyTheme(theme);
                self->m_agent->UpdateSetting(L"theme", JsonValue::CreateStringValue(theme));
            }
        });

        // The delay is sent once the slider rests, not at every step.
        auto delayTimer = DispatcherQueue().CreateTimer();
        delayTimer.Interval(std::chrono::milliseconds(300));
        delayTimer.IsRepeating(false);
        delayTimer.Tick([weak](auto&& timer, auto&&)
        {
            timer.Stop();
            if (auto self = weak.get())
            {
                self->m_agent->UpdateSetting(L"switch_delay_ms",
                    JsonValue::CreateNumberValue(std::round(self->SwitchDelaySlider().Value())));
            }
        });
        SwitchDelaySlider().ValueChanged([weak, delayTimer](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self) return;
            self->SwitchDelayValue().Text(hstring(std::format(L"{:.0f} ms", self->SwitchDelaySlider().Value())));
            if (!self->m_applying)
            {
                delayTimer.Stop();
                delayTimer.Start();
            }
        });

        PortBox().ValueChanged([weak](NumberBox const& box, auto&&)
        {
            auto self = weak.get();
            if (!self || self->m_applying) return;
            double value = box.Value();
            if (std::isnan(value))
            {
                // Emptied: back to the core's default port.
                self->m_agent->UpdateSetting(L"port", JsonValue::CreateNullValue());
                return;
            }
            self->m_agent->UpdateSetting(L"port", JsonValue::CreateNumberValue(std::round(value)));
        });

        EditHotkeyButton().Click([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->EditHotkey();
        });

        // A machine-wide setting with a LocalSystem service behind it: only
        // an administrator may change it, so the service's command line runs
        // elevated (UAC asks). On installs and starts the service, off
        // removes it. The switch waits until a snapshot shows the new state
        // (ApplySettings), or the limit below; if the user says no, it goes
        // back at once.
        ElevatedInputToggle().Toggled([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self || self->m_applying || self->m_elevatedPending) return;
            bool on = self->ElevatedInputToggle().IsOn();
            self->ElevatedInputState().Text(RinflowApp::Text(on ? L"On" : L"Off"));
            self->ElevatedInputToggle().IsEnabled(false);
            // Snapshots leave the switch alone until this is over: it would
            // flip back while UAC asks.
            self->m_elevatedPending = true;
            self->m_elevatedTarget = on;
            auto request = ++self->m_elevatedRequest;
            // The thread touches no window: only the dispatcher, which may
            // be used from anywhere.
            auto dispatcher = self->DispatcherQueue();
            std::thread([dispatcher, weak, on, request]
            {
                auto exe = wil::GetModuleFileNameW<wil::unique_cotaskmem_string>(nullptr);
                std::wstring service = exe.get();
                service.resize(service.find_last_of(L'\\') + 1);
                service += L"rinflow-service.exe";
                SHELLEXECUTEINFOW info{ sizeof(info) };
                info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI;
                info.lpVerb = L"runas";
                info.lpFile = service.c_str();
                info.lpParameters = on ? L"--elevated-input on" : L"--elevated-input off";
                info.nShow = SW_HIDE;
                if (ShellExecuteExW(&info) && info.hProcess)
                {
                    // Stopping the service may take it up to ten seconds.
                    DWORD exitCode = 1;
                    WaitForSingleObject(info.hProcess, 30000);
                    GetExitCodeProcess(info.hProcess, &exitCode);
                    CloseHandle(info.hProcess);
                    // Changed: the service starts its helper and the agent
                    // finds it within seconds; a snapshot that shows it ends
                    // the wait sooner.
                    if (exitCode == 0) Sleep(15000);
                }
                dispatcher.TryEnqueue([weak, request]
                {
                    auto self = weak.get();
                    if (!self || self->m_closed) return;
                    if (!self->m_elevatedPending || self->m_elevatedRequest != request) return;
                    self->m_elevatedPending = false;
                    // What the agent reports, whatever happened.
                    if (self->m_lastState) self->ApplySettings(self->m_lastState);
                });
            }).detach();
        });

        CopyIdButton().Click([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self || self->m_fingerprint.empty()) return;
            try
            {
                Windows::ApplicationModel::DataTransfer::DataPackage package;
                package.SetText(self->m_fingerprint);
                Windows::ApplicationModel::DataTransfer::Clipboard::SetContent(package);
                // Still there after Rinflow closes.
                Windows::ApplicationModel::DataTransfer::Clipboard::Flush();
            }
            catch (hresult_error const&)
            {
                // Another program holds the clipboard; the button can be
                // clicked again.
            }
        });

        // Checks now, or installs the update the agent has ready (setup asks
        // for an administrator, then Rinflow restarts by itself).
        CheckUpdateButton().Click([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self) return;
            if (self->m_updateReady)
            {
                // This click makes the app the foreground; passing that on
                // through the agent to setup brings the administrator
                // prompt to the front rather than flashing in the taskbar.
                AllowSetForegroundWindow(ASFW_ANY);
                self->m_agent->InstallUpdate();
            }
            else self->m_agent->CheckUpdate();
        });
        wire(AutoUpdateToggle(), AutoUpdateState(), L"auto_update");
    }

    void MainWindow::ApplyUpdate(JsonObject const& state)
    {
        auto value = state.GetNamedValue(L"update", JsonValue::CreateNullValue());
        auto update = value.ValueType() == JsonValueType::Object ? value.GetObject() : JsonObject{};
        // A missing or null field is empty (GetNamedString would throw on null).
        auto text = [&update](wchar_t const* key)
        {
            auto field = update.GetNamedValue(key, JsonValue::CreateNullValue());
            return field.ValueType() == JsonValueType::String ? std::wstring(field.GetString()) : std::wstring();
        };
        auto status = text(L"status");
        auto version = text(L"version");
        auto error = text(L"error");

        std::wstring line;
        if (status == L"checking") line = RinflowApp::Text(L"UpdateChecking");
        else if (status == L"up_to_date") line = RinflowApp::Text(L"UpdateUpToDate");
        else if (status == L"downloading")
        {
            auto progress = update.GetNamedValue(L"progress", JsonValue::CreateNullValue());
            int percent = progress.ValueType() == JsonValueType::Number
                ? static_cast<int>(std::clamp(progress.GetNumber(), 0.0, 1.0) * 100) : 0;
            line = RinflowApp::Format(L"UpdateDownloading", version, percent);
        }
        else if (status == L"ready") line = RinflowApp::Format(L"UpdateReady", version);
        else if (status == L"installing") line = RinflowApp::Format(L"UpdateInstalling", version);
        // Why the last step failed: on its own, or under 已下载 when the
        // install did not go through and the update is still ready.
        if (!error.empty()) line = line.empty() || status == L"failed" ? error : line + L"\n" + error;
        UpdateStatusText().Text(line);
        UpdateStatusText().Visibility(line.empty() ? Visibility::Collapsed : Visibility::Visible);

        m_updateReady = status == L"ready";
        CheckUpdateButton().Content(box_value(RinflowApp::Text(m_updateReady ? L"UpdateInstallButton" : L"UpdateCheckButton")));
        CheckUpdateButton().IsEnabled(status != L"checking" && status != L"downloading" && status != L"installing");

        auto notes = text(L"notes_url");
        bool showNotes = !version.empty() && notes.starts_with(L"https://");
        if (showNotes) UpdateNotesLink().NavigateUri(Windows::Foundation::Uri(notes));
        UpdateNotesLink().Visibility(showNotes ? Visibility::Visible : Visibility::Collapsed);
    }

    void MainWindow::ApplySettings(JsonObject const& state)
    {
        auto settings = state.GetNamedObject(L"settings", JsonObject{});
        m_settings = settings;
        m_applying = true;

        SetSwitch(SharingToggle(), SharingState(), settings.GetNamedBoolean(L"sharing", true));
        SetSwitch(LaunchToggle(), LaunchState(), state.GetNamedBoolean(L"launch_at_login", false));
        m_keepInTray = settings.GetNamedBoolean(L"keep_in_tray", true);
        SetSwitch(TrayToggle(), TrayState(), m_keepInTray);
        SetSwitch(CornerToggle(), CornerState(), settings.GetNamedBoolean(L"corner_guard", true));
        SetSwitch(FullscreenToggle(), FullscreenState(), settings.GetNamedBoolean(L"pause_in_fullscreen", false));
        SetSwitch(ClipboardToggle(), ClipboardState(), settings.GetNamedBoolean(L"clipboard", false));
        SetSwitch(PairedOnlyToggle(), PairedOnlyState(), settings.GetNamedBoolean(L"paired_only", true));
        bool switchHotkeys = settings.GetNamedBoolean(L"switch_hotkeys", true);
        SetSwitch(SwitchHotkeysToggle(), SwitchHotkeysState(), switchHotkeys);
        int modifiersIndex = settings.GetNamedNumber(L"switch_modifiers", 7) == 3 ? 1 : 0;
        if (SwitchModifiersCombo().SelectedIndex() != modifiersIndex)
        {
            SwitchModifiersCombo().SelectedIndex(modifiersIndex);
        }
        SwitchModifiersCombo().IsEnabled(switchHotkeys);
        // A combination another program registered first.
        SwitchHotkeysWarning().Visibility(switchHotkeys && state.GetNamedBoolean(L"switch_hotkeys_blocked", false)
            ? Visibility::Visible : Visibility::Collapsed);
        // A Mac's modifier keys: Command and Control swapped by default.
        {
            auto map = settings.GetNamedObject(L"modifier_map", JsonObject{});
            std::wstring super{ map.GetNamedString(L"super", L"control") };
            std::wstring control{ map.GetNamedString(L"control", L"super") };
            std::wstring alt{ map.GetNamedString(L"alt", L"alt") };
            bool mac = super == L"control" && control == L"super" && alt == L"alt";
            bool keep = super == L"super" && control == L"control" && alt == L"alt";
            // A map no preset matches is custom; so is one the user chose
            // 自定义 for, until they pick a preset again.
            if (!mac && !keep) m_modifierCustom = true;
            int presetIndex = m_modifierCustom ? 2 : mac ? 0 : 1;
            if (ModifierPresetCombo().SelectedIndex() != presetIndex)
            {
                ModifierPresetCombo().SelectedIndex(presetIndex);
            }
            ModifierMapDescription().Text(RinflowApp::Text(m_modifierCustom ? L"ModifierCustomDescription"
                : mac ? L"ModifierMacDescription" : L"ModifierKeepDescription"));
            ModifierCustomPanel().Visibility(m_modifierCustom ? Visibility::Visible : Visibility::Collapsed);
            for (auto const& [combo, target] : { std::pair{ ModifierSuperCombo(), super },
                std::pair{ ModifierControlCombo(), control }, std::pair{ ModifierAltCombo(), alt } })
            {
                int index = ModifierTargetIndex(target);
                if (combo.SelectedIndex() != index) combo.SelectedIndex(index);
            }
        }
        SetSwitch(KeepAwakeToggle(), KeepAwakeState(), settings.GetNamedBoolean(L"keep_awake", true));
        SetSwitch(HideCursorToggle(), HideCursorState(), settings.GetNamedBoolean(L"hide_cursor_on_leave", true));
        SetSwitch(LockSyncToggle(), LockSyncState(), settings.GetNamedBoolean(L"lock_sync", false));
        SetSwitch(AutoUpdateToggle(), AutoUpdateState(), settings.GetNamedBoolean(L"auto_update", true));

        // Encryption is not a choice: every connection is encrypted. Without
        // a fingerprint the core is not running (no certificate or port).
        auto fingerprint = state.GetNamedValue(L"fingerprint", JsonValue::CreateNullValue());
        bool encrypted = fingerprint.ValueType() == JsonValueType::String && !fingerprint.GetString().empty();
        SetSwitch(TlsToggle(), TlsState(), encrypted);
        TlsDescription().Text(encrypted
            ? RinflowApp::Text(L"TlsOn")
            : RinflowApp::Text(L"TlsOff"));

        auto theme = settings.GetNamedString(L"theme", L"system");
        int themeIndex = theme == L"light" ? 1 : theme == L"dark" ? 2 : 0;
        if (ThemeCombo().SelectedIndex() != themeIndex)
        {
            ThemeCombo().SelectedIndex(themeIndex);
        }
        auto language = settings.GetNamedString(L"language", L"system");
        int languageIndex = language == L"zh-CN" ? 1 : language == L"en-US" ? 2 : language == L"ja-JP" ? 3 : 0;
        if (LanguageCombo().SelectedIndex() != languageIndex)
        {
            LanguageCombo().SelectedIndex(languageIndex);
        }
        ApplyTheme(theme);

        // Not while the user works the slider: the snapshot may still carry
        // the value from before.
        double delay = settings.GetNamedNumber(L"switch_delay_ms", 150);
        if (SwitchDelaySlider().Value() != delay && SwitchDelaySlider().FocusState() == FocusState::Unfocused)
        {
            SwitchDelaySlider().Value(delay);
        }
        SwitchDelayValue().Text(hstring(std::format(L"{:.0f} ms", SwitchDelaySlider().Value())));

        auto hotkey = settings.GetNamedObject(L"lock_hotkey", JsonObject{});
        auto modifiers = static_cast<uint32_t>(hotkey.GetNamedNumber(L"modifiers", DefaultModifiers));
        auto key = static_cast<uint32_t>(hotkey.GetNamedNumber(L"key", DefaultKey));
        ShowHotkeyKeys(HotkeyKeys(), modifiers, key);
        std::wstring hotkeyText;
        for (auto const& part : HotkeyParts(modifiers, key))
        {
            hotkeyText += (hotkeyText.empty() ? L"" : L"+") + std::wstring(part);
        }
        bool locked = state.GetNamedBoolean(L"locked", false);
        LockBar().Message(RinflowApp::Format(L"LockBarMessage", hotkeyText));
        LockBar().IsOpen(locked);

        auto running = static_cast<uint32_t>(state.GetNamedNumber(L"control_port", 0));
        auto portValue = settings.GetNamedValue(L"port", JsonValue::CreateNullValue());
        uint32_t configured = portValue.ValueType() == JsonValueType::Number
            ? static_cast<uint32_t>(portValue.GetNumber())
            : running;
        if (PortBox().FocusState() == FocusState::Unfocused && PortBox().Value() != configured)
        {
            PortBox().Value(configured);
        }
        PortHint().Text(configured != running
            ? RinflowApp::Format(L"PortPending", configured, running)
            : RinflowApp::Text(L"PortHint"));

        // The input service: running while the feature is on. Installed with
        // setup, the switch can add it; a portable copy cannot.
        auto elevated = state.GetNamedObject(L"elevated_input", JsonObject{});
        bool elevatedAvailable = elevated.GetNamedBoolean(L"available", false);
        bool elevatedInstallable = elevated.GetNamedBoolean(L"installable", false);
        bool elevatedOn = elevatedAvailable && elevated.GetNamedBoolean(L"enabled", false);
        // The change took effect: no need to wait for the limit.
        if (m_elevatedPending && elevatedOn == m_elevatedTarget)
        {
            m_elevatedPending = false;
        }
        if (!m_elevatedPending)
        {
            SetSwitch(ElevatedInputToggle(), ElevatedInputState(), elevatedOn);
            ElevatedInputToggle().IsEnabled(elevatedAvailable || elevatedInstallable);
        }
        ElevatedInputDescription().Text(elevatedOn
            ? RinflowApp::Text(L"ElevatedOn")
            : elevatedAvailable || elevatedInstallable
            ? RinflowApp::Text(L"ElevatedRecommended")
            : RinflowApp::Text(L"ElevatedOff"));

        // The first four and last three bytes; the tooltip and 复制 give all 32.
        m_fingerprint = encrypted ? hstring(ColonHex(fingerprint.GetString())) : hstring();
        auto full = std::wstring(m_fingerprint);
        DeviceIdText().Text(full.size() > 24
            ? hstring(full.substr(0, 11) + L" … " + full.substr(full.size() - 8))
            : hstring(full.empty() ? L"—" : full));
        ToolTipService::SetToolTip(DeviceIdText(), full.empty() ? nullptr : box_value(m_fingerprint));
        CopyIdButton().IsEnabled(!full.empty());
        VersionText().Text(RinflowApp::Format(L"Version", std::wstring(state.GetNamedString(L"agent_version", L"—"))));
        ApplyUpdate(state);

        m_applying = false;
    }

    void MainWindow::ShowHotkeyKeys(StackPanel const& panel, uint32_t modifiers, uint32_t key)
    {
        panel.Children().Clear();
        auto keycap = Application::Current().Resources().Lookup(box_value(L"KeycapStyle")).as<Style>();
        for (auto const& part : HotkeyParts(modifiers, key))
        {
            Border border;
            border.Style(keycap);
            TextBlock text;
            text.Text(part);
            text.HorizontalAlignment(HorizontalAlignment::Center);
            border.Child(text);
            panel.Children().Append(border);
        }
    }

    fire_and_forget MainWindow::EditHotkey()
    {
        auto strong = get_strong();
        auto hotkey = m_settings ? m_settings.GetNamedObject(L"lock_hotkey", JsonObject{}) : JsonObject{};
        m_newHotkeyModifiers = static_cast<uint32_t>(hotkey.GetNamedNumber(L"modifiers", DefaultModifiers));
        m_newHotkeyKey = static_cast<uint32_t>(hotkey.GetNamedNumber(L"key", DefaultKey));
        ShowHotkeyKeys(HotkeyPreview(), m_newHotkeyModifiers, m_newHotkeyKey);
        HotkeyHint().Text(L"");
        HotkeyDialog().IsPrimaryButtonEnabled(true);

        auto weak = get_weak();
        auto token = HotkeyDialog().PreviewKeyDown([weak](auto&&, KeyRoutedEventArgs const& args)
        {
            auto self = weak.get();
            if (!self) return;
            auto vk = static_cast<uint32_t>(args.Key());
            uint32_t modifiers = 0;
            if (GetKeyState(VK_CONTROL) < 0) modifiers |= ModControl;
            if (GetKeyState(VK_MENU) < 0) modifiers |= ModAlt;
            if (GetKeyState(VK_SHIFT) < 0) modifiers |= ModShift;
            if (GetKeyState(VK_LWIN) < 0 || GetKeyState(VK_RWIN) < 0) modifiers |= ModWin;
            // Plain Tab, Enter and Esc keep working the dialog.
            if (modifiers == 0 && (vk == VK_TAB || vk == VK_RETURN || vk == VK_ESCAPE))
            {
                return;
            }
            args.Handled(true);
            uint32_t key = IsModifierKey(vk) ? 0 : vk;
            self->m_newHotkeyModifiers = modifiers;
            self->m_newHotkeyKey = key;
            self->ShowHotkeyKeys(self->HotkeyPreview(), modifiers, key);
            bool valid = IsValidHotkey(modifiers, key);
            self->HotkeyDialog().IsPrimaryButtonEnabled(valid);
            self->HotkeyHint().Text(valid || key == 0 ? hstring() : RinflowApp::Text(L"HotkeyNeedsModifier"));
        });

        auto result = ContentDialogResult::None;
        try
        {
            HotkeyDialog().XamlRoot(Root().XamlRoot());
            result = co_await HotkeyDialog().ShowAsync();
        }
        catch (hresult_error const&)
        {
            // Another dialog is open (or still closing): nothing changes.
        }
        HotkeyDialog().PreviewKeyDown(token);
        if (m_closed)
        {
            co_return;
        }
        if (result == ContentDialogResult::Primary && IsValidHotkey(m_newHotkeyModifiers, m_newHotkeyKey))
        {
            m_agent->UpdateSetting(L"lock_hotkey", HotkeyValue(m_newHotkeyModifiers, m_newHotkeyKey));
        }
        else if (result == ContentDialogResult::Secondary)
        {
            m_agent->UpdateSetting(L"lock_hotkey", HotkeyValue(DefaultModifiers, DefaultKey));
        }
    }
}
