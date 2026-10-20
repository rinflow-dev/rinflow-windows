#include "pch.h"
#include "MainWindow.xaml.h"
#if __has_include("MainWindow.g.cpp")
#include "MainWindow.g.cpp"
#endif
#include "PeerItem.h"

// Fully qualified: WIL declares a global ::Microsoft namespace as well.
using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;
using namespace winrt::Windows::Data::Json;

namespace
{
    hstring PlatformLabel(hstring const& platform)
    {
        if (platform == L"macos") return L"Mac";
        if (platform == L"windows") return L"Windows";
        if (platform == L"linux") return L"Linux";
        if (platform == L"android") return L"Android";
        return L"未知设备";
    }
}

namespace winrt::LinkCursor::implementation
{
    void MainWindow::InitializeComponent()
    {
        MainWindowT::InitializeComponent();

        ExtendsContentIntoTitleBar(true);
        SetTitleBar(AppTitleBar());

        HWND hwnd{};
        check_hresult(m_inner.as<::IWindowNative>()->get_WindowHandle(&hwnd));
        double scale = GetDpiForWindow(hwnd) / 96.0;
        AppWindow().Resize({ static_cast<int32_t>(720 * scale), static_cast<int32_t>(760 * scale) });

        auto dispatcher = DispatcherQueue();
        auto weak = get_weak();
        m_agent = std::make_unique<::LinkCursorApp::AgentClient>();
        m_agent->Start(
            [dispatcher, weak](JsonObject const& message)
            {
                dispatcher.TryEnqueue([weak, message]
                {
                    if (auto self = weak.get()) self->ApplyMessage(message);
                });
            },
            [dispatcher, weak](bool linked)
            {
                dispatcher.TryEnqueue([weak, linked]
                {
                    if (auto self = weak.get()) self->SetAgentLinked(linked);
                });
            });

        SetupSections();
        SetupEffectControls();

        Closed([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->m_agent->Stop();
        });
    }

    void MainWindow::SetupSections()
    {
        auto weak = get_weak();
        Sections().SelectionChanged([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self) return;
            bool effect = self->Sections().SelectedItem() == self->EffectItem();
            self->DevicesPanel().Visibility(effect ? Visibility::Collapsed : Visibility::Visible);
            self->EffectPanel().Visibility(effect ? Visibility::Visible : Visibility::Collapsed);
        });
    }

    void MainWindow::SetupEffectControls()
    {
        auto number = [](std::wstring_view unit, double multiplier = 1.0, int decimals = 0)
        {
            return [unit, multiplier, decimals](double value)
            {
                auto text = decimals == 0
                    ? std::format(L"{:.0f}", value * multiplier)
                    : std::format(L"{:.2f}", value * multiplier);
                return unit.empty() ? text : text + L" " + std::wstring(unit);
            };
        };
        // Keys and units match EffectParams in agent/src/effect.rs.
        m_effectSliders = {
            { L"push_distance", PushDistanceSlider(), PushDistanceValue(), number(L"") },
            { L"release_delay", ReleaseDelaySlider(), ReleaseDelayValue(), number(L"毫秒", 1000) },
            { L"warm_seconds", WarmSecondsSlider(), WarmSecondsValue(), number(L"秒") },
            { L"corner_dead_zone", CornerDeadZoneSlider(), CornerDeadZoneValue(), number(L"pt") },
            { L"spring_response", SpringResponseSlider(), SpringResponseValue(), number(L"秒", 1, 2) },
            { L"spring_damping", SpringDampingSlider(), SpringDampingValue(), number(L"", 1, 2) },
            { L"bump_depth", BumpDepthSlider(), BumpDepthValue(), number(L"pt") },
            { L"bump_width", BumpWidthSlider(), BumpWidthValue(), number(L"pt") },
            { L"dot_size", DotSizeSlider(), DotSizeValue(), number(L"pt") },
            { L"opacity", OpacitySlider(), OpacityValue(), number(L"%", 100) },
        };

        auto weak = get_weak();
        auto edited = [weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->OnEffectEdited();
        };
        for (auto const& entry : m_effectSliders)
        {
            entry.slider.ValueChanged(edited);
        }
        RippleToggle().Toggled(edited);
        ThemeCombo().SelectionChanged(edited);

        DemoToggle().Toggled([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (self && !self->m_applyingEffect)
            {
                self->m_agent->SetEffectDemo(self->DemoToggle().IsOn());
            }
        });
        PreviewButton().Click([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->m_agent->PreviewEffect();
        });
        ResetEffectButton().Click([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->m_agent->ResetEffect();
        });
        UpdateEffectLabels();
    }

    void MainWindow::OnEffectEdited()
    {
        UpdateEffectLabels();
        if (m_applyingEffect)
        {
            return;
        }
        JsonObject params;
        for (auto const& entry : m_effectSliders)
        {
            params.Insert(entry.key, JsonValue::CreateNumberValue(entry.slider.Value()));
        }
        params.Insert(L"ripple", JsonValue::CreateBooleanValue(RippleToggle().IsOn()));
        hstring theme = L"auto";
        if (auto item = ThemeCombo().SelectedItem().try_as<Controls::ComboBoxItem>())
        {
            theme = unbox_value_or<hstring>(item.Tag(), L"auto");
        }
        params.Insert(L"theme", JsonValue::CreateStringValue(theme));
        m_agent->SetEffect(params);
    }

    void MainWindow::UpdateEffectLabels()
    {
        for (auto const& entry : m_effectSliders)
        {
            entry.label.Text(entry.format(entry.slider.Value()));
        }
    }

    void MainWindow::ApplyEffect(JsonObject const& state)
    {
        m_applyingEffect = true;

        // Values are copied only when the agent reloads them (start-up,
        // reset). Copying every snapshot would fight the slider being dragged.
        auto revision = state.GetNamedNumber(L"effect_revision", 0);
        if (revision != m_effectRevision)
        {
            m_effectRevision = revision;
            auto params = state.GetNamedObject(L"effect", JsonObject{});
            for (auto const& entry : m_effectSliders)
            {
                entry.slider.Value(params.GetNamedNumber(entry.key, entry.slider.Value()));
            }
            RippleToggle().IsOn(params.GetNamedBoolean(L"ripple", true));
            auto theme = params.GetNamedString(L"theme", L"auto");
            ThemeCombo().SelectedIndex(theme == L"light" ? 1 : theme == L"dark" ? 2 : 0);
            UpdateEffectLabels();
        }

        bool demo = state.GetNamedBoolean(L"effect_demo", false);
        if (DemoToggle().IsOn() != demo)
        {
            DemoToggle().IsOn(demo);
        }
        auto side = state.GetNamedString(L"demo_side", L"local");
        DemoSideText().Text(side == L"remote"
            ? L"现在在“另一台设备”上 · 往左推回到本机"
            : L"现在在本机 · 往右推进入另一台设备");
        DemoSideText().Visibility(demo ? Visibility::Visible : Visibility::Collapsed);

        m_applyingEffect = false;
    }

    void MainWindow::OnPeerAction(Windows::Foundation::IInspectable const& sender, RoutedEventArgs const&)
    {
        auto peerId = unbox_value_or<hstring>(sender.as<Controls::Button>().Tag(), L"");
        if (peerId.empty())
        {
            return;
        }
        if (peerId == m_connectedPeerId)
        {
            m_agent->Disconnect();
        }
        else
        {
            m_agent->Connect(peerId);
        }
    }

    void MainWindow::ApplyMessage(JsonObject const& message)
    {
        auto type = message.GetNamedString(L"type", L"");
        if (type == L"state")
        {
            ApplyState(message);
        }
        else if (type == L"error")
        {
            ShowError(message.GetNamedString(L"message", L""));
        }
    }

    void MainWindow::ApplyState(JsonObject const& state)
    {
        DeviceNameText().Text(state.GetNamedString(L"device_name", L"LinkCursor"));

        auto connection = state.GetNamedObject(L"connection", JsonObject{});
        auto status = connection.GetNamedString(L"status", L"disconnected");
        hstring connectingPeerId;
        m_connectedPeerId = L"";
        if (status == L"connected")
        {
            auto peer = connection.GetNamedObject(L"peer", JsonObject{});
            m_connectedPeerId = peer.GetNamedString(L"id", L"");
            auto name = peer.GetNamedString(L"name", L"");
            StatusText().Text(state.GetNamedBoolean(L"remote_control", false)
                ? L"正在被 " + name + L" 控制键盘和鼠标"
                : L"已连接到 " + name);
        }
        else if (status == L"connecting")
        {
            connectingPeerId = connection.GetNamedString(L"peer_id", L"");
            StatusText().Text(L"正在连接…");
        }
        else
        {
            auto core = state.GetNamedString(L"core", L"starting");
            StatusText().Text(core == L"running" ? L"未连接 · 正在局域网中查找设备"
                : core == L"stopped" ? L"核心服务已停止"
                : L"正在启动…");
        }

        ShowError(state.GetNamedString(L"last_error", L""));

        m_peers.Clear();
        for (auto const& value : state.GetNamedArray(L"peers", JsonArray{}))
        {
            auto peer = value.GetObject();
            auto id = peer.GetNamedString(L"id", L"");
            auto detail = PlatformLabel(peer.GetNamedString(L"platform", L""))
                + L" · " + peer.GetNamedString(L"address", L"");
            hstring action = L"连接";
            bool canAct = true;
            if (id == m_connectedPeerId)
            {
                action = L"断开";
            }
            else if (id == connectingPeerId)
            {
                action = L"连接中…";
                canAct = false;
            }
            m_peers.Append(make<PeerItem>(id, peer.GetNamedString(L"name", id), detail, action, canAct));
        }
        EmptyText().Visibility(m_peers.Size() == 0 ? Visibility::Visible : Visibility::Collapsed);

        ApplyEffect(state);
    }

    void MainWindow::SetAgentLinked(bool linked)
    {
        if (!linked)
        {
            StatusText().Text(L"正在连接后台服务…");
            m_peers.Clear();
            m_connectedPeerId = L"";
            EmptyText().Visibility(Visibility::Visible);
            // A restarted agent may have different values; reload them.
            m_effectRevision = -1;
        }
    }

    void MainWindow::ShowError(hstring const& message)
    {
        ErrorBar().Message(message);
        ErrorBar().IsOpen(!message.empty());
    }
}
