#pragma once

#include "MainWindow.g.h"
#include "AgentClient.h"
#include "DeviceModel.h"

namespace winrt::LinkCursor::implementation
{
    struct MainWindow : MainWindowT<MainWindow>
    {
        MainWindow()
        {
            // Xaml objects should not call InitializeComponent during construction.
            // See https://github.com/microsoft/cppwinrt/tree/master/nuget#initializecomponent
        }

        // Named elements only exist after the base InitializeComponent runs.
        void InitializeComponent();

        Windows::Foundation::Collections::IObservableVector<Windows::Foundation::IInspectable> Devices() const
        {
            return m_devices;
        }
        Windows::Foundation::Collections::IObservableVector<Windows::Foundation::IInspectable> Nearby() const
        {
            return m_nearby;
        }

        enum class Page { Devices, Settings, Effect };
        // Shows a page and brings the window to the front.
        void ShowPage(Page page);

        void OnConnectDevice(Windows::Foundation::IInspectable const& sender, Microsoft::UI::Xaml::RoutedEventArgs const& args);
        void OnDeviceMore(Windows::Foundation::IInspectable const& sender, Microsoft::UI::Xaml::RoutedEventArgs const& args);

        // ---- Arrangement (MainWindow.Arrangement.cpp) ----------------------

        // A rectangle in logical points.
        struct LayoutRect
        {
            double x{}, y{}, width{}, height{};
            double scale{ 1.0 };
            double Right() const { return x + width; }
            double Bottom() const { return y + height; }
        };

        // A peer as the arrangement draws it.
        struct LayoutDevice
        {
            hstring peerId;
            hstring name;
            hstring platform;
            std::vector<LayoutRect> displays; // in the peer's own space
            LayoutRect bounds;                // of `displays`
            std::optional<std::pair<double, double>> offset;
            hstring side;
            bool remembered{ false };
        };

        // A device being dragged: a placed peer, a peer from its chip, or
        // this PC (`local`, which moves every placed peer the other way).
        struct LayoutDrag
        {
            hstring peerId;
            Microsoft::UI::Xaml::FrameworkElement element{ nullptr };
            Windows::Foundation::Point grab{};  // pointer offset inside the element
            Windows::Foundation::Point start{}; // where the pointer went down
            bool fromChip{ false };
            bool local{ false };
        };

    private:
        // ---- Window, state, devices (MainWindow.xaml.cpp) -----------------
        void ApplyMessage(Windows::Data::Json::JsonObject const& message);
        void ApplyState(Windows::Data::Json::JsonObject const& state);
        void ApplyDevices(Windows::Data::Json::JsonObject const& state);
        void ApplyTheme(hstring const& theme);
        void SetAgentLinked(bool linked);
        // Says why the background service is not running, with a retry.
        void ShowAgentProblem(::LinkCursorApp::AgentProblem problem, uint32_t code);
        // `fromAgent`: the agent's last_error, which closing the bar clears.
        void ShowError(hstring const& message, bool fromAgent);
        void SetupDevicesPage();
        void ShowAddDevice();
        Microsoft::UI::Xaml::Controls::MenuFlyout DeviceMenu(LinkCursorApp::DeviceInfo const& device);

        // ---- Arrangement (MainWindow.Arrangement.cpp) ----------------------
        void SetupArrangement();
        void ApplyLayout(Windows::Data::Json::JsonObject const& state);
        void RenderArrangement();
        void RenderDots();
        // One screen card; `insets` draw it inside its to-scale bounds.
        Microsoft::UI::Xaml::FrameworkElement Screen(hstring const& templateKey, double width, double height,
            Microsoft::UI::Xaml::Thickness const& insets, Microsoft::UI::Xaml::CornerRadius const& corners,
            hstring const& glyph, hstring const& title, hstring const& subtitle);
        Microsoft::UI::Xaml::FrameworkElement DeviceElement(LayoutDevice const& device, bool ghost);
        // Makes a placed peer (or this PC, `local`) draggable.
        void AttachDrag(Microsoft::UI::Xaml::FrameworkElement const& element, hstring const& peerId, bool local);
        void BeginDrag(hstring const& peerId, bool local, Microsoft::UI::Xaml::FrameworkElement const& element,
            Microsoft::UI::Xaml::Input::PointerRoutedEventArgs const& args);
        // The placed peer nearest to this PC moved by (dx, dy).
        LayoutDevice const* NearestPlaced(double dx, double dy) const;
        void MoveDrag(Microsoft::UI::Xaml::Input::PointerRoutedEventArgs const& args);
        void EndDrag(Microsoft::UI::Xaml::Input::PointerRoutedEventArgs const& args);
        LayoutDevice const* FindLayoutDevice(hstring const& peerId) const;
        // The offset the dragged device is at now (this PC's space).
        std::pair<double, double> DraggedOffset() const;
        // Asks the agent where the dragged device would land; one request in
        // flight, the latest position sent when it is answered.
        void RequestPreview();
        void ApplyPlacementPreview(Windows::Data::Json::JsonObject const& message);
        void ClearPreview();

        // ---- Pairing (MainWindow.Pairing.cpp) -----------------------------
        void SetupPairing();
        void ApplyPairing(Windows::Data::Json::JsonObject const& state);
        hstring m_pairingPeer;
        bool m_pairingOpen{ false };
        // Set while the dialog is closed because the pairing ended.
        bool m_pairingClosing{ false };
        Microsoft::UI::Dispatching::DispatcherQueueTimer m_pairingRetry{ nullptr };
        Windows::Data::Json::JsonObject m_lastState{ nullptr };

        // ---- Transfers, addresses, per-device input (MainWindow.Transfers.cpp)
        void SetupTransfers();
        void ApplyTransfers(Windows::Data::Json::JsonObject const& state);
        void ApplyAddresses(Windows::Data::Json::JsonObject const& state);
        void AddAddress();
        std::vector<std::wstring> ManualAddresses() const;
        void SaveAddresses(std::vector<std::wstring> const& addresses);
        fire_and_forget EditPeerInput(hstring peerId, hstring name);
        struct TransferRow
        {
            Microsoft::UI::Xaml::Controls::ProgressBar bar{ nullptr };
            Microsoft::UI::Xaml::Controls::TextBlock detail{ nullptr };
        };
        // By direction and transfer number.
        std::map<std::wstring, TransferRow> m_transferRows;
        std::wstring m_transferSignature;
        std::wstring m_addressSignature;
        bool m_peerInputOpen{ false };

        // ---- Settings (MainWindow.Settings.cpp) ----------------------------
        void SetupSettingsPage();
        void ApplySettings(Windows::Data::Json::JsonObject const& state);
        void ShowHotkeyKeys(Microsoft::UI::Xaml::Controls::StackPanel const& panel, uint32_t modifiers, uint32_t key);
        fire_and_forget EditHotkey();

        // ---- Edge animation, development builds (MainWindow.Effect.cpp) ---
        // One tunable effect parameter: its key in the agent's JSON, the
        // slider that edits it and the label that shows the value.
        struct EffectSlider
        {
            std::wstring_view key;
            Microsoft::UI::Xaml::Controls::Slider slider{ nullptr };
            Microsoft::UI::Xaml::Controls::TextBlock label{ nullptr };
            std::function<std::wstring(double)> format;
        };
        void SetupEffectPage();
        void ApplyEffect(Windows::Data::Json::JsonObject const& state);
        void OnEffectEdited();
        void UpdateEffectLabels();

        std::unique_ptr<::LinkCursorApp::AgentClient> m_agent;
        // Set once the window closed: messages still queued are dropped.
        bool m_closed{ false };
        // The agent's last_error as last seen; shown when it changes, so a
        // dismissed error does not come back with the next snapshot.
        hstring m_agentError;
        bool m_errorFromAgent{ false };
        bool m_elevatedPending{ false };
        Page m_page{ Page::Devices };
        LinkCursorApp::DeviceModel m_model;
        Windows::Foundation::Collections::IObservableVector<Windows::Foundation::IInspectable> m_devices{
            single_threaded_observable_vector<Windows::Foundation::IInspectable>()
        };
        Windows::Foundation::Collections::IObservableVector<Windows::Foundation::IInspectable> m_nearby{
            single_threaded_observable_vector<Windows::Foundation::IInspectable>()
        };
        std::wstring m_devicesSignature;
        std::wstring m_nearbySignature;
        bool m_addDeviceOpen{ false };

        // Set while state from the agent is copied into the controls, so the
        // resulting change events are not echoed back as edits.
        bool m_applying{ false };
        Windows::Data::Json::JsonObject m_settings{ nullptr };
        hstring m_theme;
        bool m_keepInTray{ true };
        hstring m_deviceId;
        // This PC's certificate fingerprint, as "3F:A2:…" (empty without QUIC).
        hstring m_fingerprint;

        std::vector<LayoutRect> m_localDisplays;
        std::vector<LayoutDevice> m_layoutDevices;
        std::set<std::wstring> m_onlinePeerIds;
        std::set<std::wstring> m_connectedPeerIds;
        std::wstring m_layoutSignature;
        // Canvas pixels per logical point, and the logical point at the
        // canvas origin.
        double m_layoutScale{ 1.0 };
        double m_layoutOriginX{ 0.0 };
        double m_layoutOriginY{ 0.0 };
        std::optional<LayoutDrag> m_drag;
        bool m_layoutDirty{ false };
        // The landing spot shown while dragging.
        Microsoft::UI::Xaml::FrameworkElement m_preview{ nullptr };
        // The peer the last preview was asked for (the nearest one while
        // this PC is dragged).
        hstring m_previewPeer;
        bool m_previewPending{ false };
        bool m_previewQueued{ false };
        hstring m_localName;

        std::vector<EffectSlider> m_effectSliders;
        bool m_applyingEffect{ false };
        double m_effectRevision{ -1 };

        // The shortcut being recorded in the hotkey dialog.
        uint32_t m_newHotkeyModifiers{ 0 };
        uint32_t m_newHotkeyKey{ 0 };
    };
}

namespace winrt::LinkCursor::factory_implementation
{
    struct MainWindow : MainWindowT<MainWindow, implementation::MainWindow>
    {
    };
}
