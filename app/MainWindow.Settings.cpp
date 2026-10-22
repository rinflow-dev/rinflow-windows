// The 设置 page. Values come from the snapshot's `settings` (see
// agent/src/settings.rs); every edit goes back as `update_settings`.

#include "pch.h"
#include "MainWindow.xaml.h"

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

    constexpr wchar_t ReleasesPage[] = L"https://github.com/akarineren/linkcursor-windows/releases";

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
        state.Text(on ? L"开" : L"关");
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
}

namespace winrt::LinkCursor::implementation
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
                state.Text(toggle.IsOn() ? L"开" : L"关");
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

        // A machine-wide setting: only an administrator may change it, so
        // the service's command line runs elevated (UAC asks). The helper
        // notices within seconds and the next snapshot shows the result;
        // until then, and if the user says no, the switch goes back.
        ElevatedInputToggle().Toggled([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self || self->m_applying || self->m_elevatedPending) return;
            bool on = self->ElevatedInputToggle().IsOn();
            self->ElevatedInputState().Text(on ? L"开" : L"关");
            self->ElevatedInputToggle().IsEnabled(false);
            // Snapshots leave the switch alone until this is over: it would
            // flip back while UAC asks.
            self->m_elevatedPending = true;
            // The thread touches no window: only the dispatcher, which may
            // be used from anywhere.
            auto dispatcher = self->DispatcherQueue();
            std::thread([dispatcher, weak, on]
            {
                auto exe = wil::GetModuleFileNameW<wil::unique_cotaskmem_string>(nullptr);
                std::wstring service = exe.get();
                service.resize(service.find_last_of(L'\\') + 1);
                service += L"linkcursor-service.exe";
                SHELLEXECUTEINFOW info{ sizeof(info) };
                info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI;
                info.lpVerb = L"runas";
                info.lpFile = service.c_str();
                info.lpParameters = on ? L"--elevated-input on" : L"--elevated-input off";
                info.nShow = SW_HIDE;
                if (ShellExecuteExW(&info) && info.hProcess)
                {
                    DWORD exitCode = 1;
                    WaitForSingleObject(info.hProcess, 10000);
                    GetExitCodeProcess(info.hProcess, &exitCode);
                    CloseHandle(info.hProcess);
                    // Changed: the input helper reads the setting every two
                    // seconds; wait for it rather than show the old value.
                    if (exitCode == 0) Sleep(2500);
                }
                dispatcher.TryEnqueue([weak]
                {
                    auto self = weak.get();
                    if (!self || self->m_closed) return;
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
                // Still there after LinkCursor closes.
                Windows::ApplicationModel::DataTransfer::Clipboard::Flush();
            }
            catch (hresult_error const&)
            {
                // Another program holds the clipboard; the button can be
                // clicked again.
            }
        });

        CheckUpdateButton().Click([](auto&&, auto&&)
        {
            ShellExecuteW(nullptr, L"open", ReleasesPage, nullptr, nullptr, SW_SHOWNORMAL);
        });
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

        // Encryption is not a choice: every connection is encrypted. Without
        // a fingerprint the core is not running (no certificate or port).
        auto fingerprint = state.GetNamedValue(L"fingerprint", JsonValue::CreateNullValue());
        bool encrypted = fingerprint.ValueType() == JsonValueType::String && !fingerprint.GetString().empty();
        SetSwitch(TlsToggle(), TlsState(), encrypted);
        TlsDescription().Text(encrypted
            ? L"所有连接都用 TLS 1.3 加密，并核对对方的证书。"
            : L"LinkCursor 没有启动：本机证书或端口无法使用，暂时连接不了其他设备。");

        auto theme = settings.GetNamedString(L"theme", L"system");
        int themeIndex = theme == L"light" ? 1 : theme == L"dark" ? 2 : 0;
        if (ThemeCombo().SelectedIndex() != themeIndex)
        {
            ThemeCombo().SelectedIndex(themeIndex);
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
        LockBar().Message(hstring(L"光标不会切换到其他设备。按 " + hotkeyText + L" 解除。"));
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
            ? hstring(std::format(L"重新启动 LinkCursor 后改用 {}；现在使用 {}。", configured, running))
            : hstring(L"其他设备通过这个端口连接本机。修改后需要重新启动 LinkCursor。"));

        // The input service: present only when installed.
        auto elevated = state.GetNamedObject(L"elevated_input", JsonObject{});
        bool elevatedAvailable = elevated.GetNamedBoolean(L"available", false);
        if (!m_elevatedPending)
        {
            SetSwitch(ElevatedInputToggle(), ElevatedInputState(), elevatedAvailable && elevated.GetNamedBoolean(L"enabled", false));
            ElevatedInputToggle().IsEnabled(elevatedAvailable);
        }
        ElevatedInputDescription().Text(elevatedAvailable
            ? L"其他设备控制本机时，也能操作以管理员身份运行的程序、UAC 确认窗口和锁屏。更改需要管理员确认。"
            : L"需要用安装包安装 LinkCursor：由随附的输入服务完成。");

        // The first four and last three bytes; the tooltip and 复制 give all 32.
        m_fingerprint = encrypted ? hstring(ColonHex(fingerprint.GetString())) : hstring();
        auto full = std::wstring(m_fingerprint);
        DeviceIdText().Text(full.size() > 24
            ? hstring(full.substr(0, 11) + L" … " + full.substr(full.size() - 8))
            : hstring(full.empty() ? L"—" : full));
        ToolTipService::SetToolTip(DeviceIdText(), full.empty() ? nullptr : box_value(m_fingerprint));
        CopyIdButton().IsEnabled(!full.empty());
        VersionText().Text(L"版本 " + state.GetNamedString(L"agent_version", L"—"));

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
            self->HotkeyHint().Text(valid || key == 0 ? L"" : L"需要包含 Ctrl、Alt 或 Win");
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
