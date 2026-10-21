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

        // A device being dragged: from the canvas (placed) or a chip.
        struct LayoutDrag
        {
            hstring peerId;
            Microsoft::UI::Xaml::FrameworkElement element{ nullptr };
            Windows::Foundation::Point grab{};  // pointer offset inside the element
            Windows::Foundation::Point start{}; // where the pointer went down
            bool fromChip{ false };
        };

    private:
        // ---- Window, state, devices (MainWindow.xaml.cpp) -----------------
        void ApplyMessage(Windows::Data::Json::JsonObject const& message);
        void ApplyState(Windows::Data::Json::JsonObject const& state);
        void ApplyDevices(Windows::Data::Json::JsonObject const& state);
        void ApplyTheme(hstring const& theme);
        void SetAgentLinked(bool linked);
        void ShowError(hstring const& message);
        void SetupDevicesPage();
        void ShowAddDevice();
        Microsoft::UI::Xaml::Controls::MenuFlyout DeviceMenu(LinkCursorApp::DeviceInfo const& device);

        // ---- Arrangement (MainWindow.Arrangement.cpp) ----------------------
        void SetupArrangement();
        void ApplyLayout(Windows::Data::Json::JsonObject const& state);
        void RenderArrangement();
        void RenderDots();
        Microsoft::UI::Xaml::FrameworkElement Screen(hstring const& templateKey, double width, double height,
            hstring const& glyph, hstring const& title, hstring const& subtitle, bool dashed, bool strong);
        Microsoft::UI::Xaml::FrameworkElement DeviceElement(LayoutDevice const& device, bool ghost);
        void BeginDrag(LayoutDevice const& device, Microsoft::UI::Xaml::FrameworkElement const& element,
            Microsoft::UI::Xaml::Input::PointerRoutedEventArgs const& args);
        void MoveDrag(Microsoft::UI::Xaml::Input::PointerRoutedEventArgs const& args);
        void EndDrag(Microsoft::UI::Xaml::Input::PointerRoutedEventArgs const& args);
        LayoutDevice const* FindLayoutDevice(hstring const& peerId) const;

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
