#pragma once

#include "MainWindow.g.h"
#include "AgentClient.h"

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

        Windows::Foundation::Collections::IObservableVector<Windows::Foundation::IInspectable> Peers() const
        {
            return m_peers;
        }

        void OnPeerAction(Windows::Foundation::IInspectable const& sender, Microsoft::UI::Xaml::RoutedEventArgs const& args);
        void OnPlaceLeft(Windows::Foundation::IInspectable const& sender, Microsoft::UI::Xaml::RoutedEventArgs const& args);
        void OnPlaceRight(Windows::Foundation::IInspectable const& sender, Microsoft::UI::Xaml::RoutedEventArgs const& args);
        void OnPlaceTop(Windows::Foundation::IInspectable const& sender, Microsoft::UI::Xaml::RoutedEventArgs const& args);
        void OnPlaceBottom(Windows::Foundation::IInspectable const& sender, Microsoft::UI::Xaml::RoutedEventArgs const& args);
        void OnRemovePlacement(Windows::Foundation::IInspectable const& sender, Microsoft::UI::Xaml::RoutedEventArgs const& args);

        // ---- Arrangement page (MainWindow.Arrangement.cpp) ----------------

        // A rectangle in logical points.
        struct LayoutRect
        {
            double x{}, y{}, width{}, height{};
            double Right() const { return x + width; }
            double Bottom() const { return y + height; }
        };

        // A peer as the arrangement page draws it.
        struct LayoutDevice
        {
            hstring peerId;
            hstring name;
            hstring platform;
            std::vector<LayoutRect> displays; // in the peer's own space
            LayoutRect bounds;                // of `displays`
            std::optional<std::pair<double, double>> offset;
            hstring side;
        };

        // A device being dragged: from the canvas (placed) or from the tray.
        struct LayoutDrag
        {
            hstring peerId;
            Microsoft::UI::Xaml::FrameworkElement element{ nullptr };
            Windows::Foundation::Point grab{};   // pointer offset inside the element
            bool fromTray{ false };
        };

    private:
        void SetupArrangement();
        void ApplyLayout(Windows::Data::Json::JsonObject const& state);
        void RenderArrangement();
        Microsoft::UI::Xaml::FrameworkElement DeviceElement(LayoutDevice const& device, bool ghost);
        void BeginDrag(LayoutDevice const& device, Microsoft::UI::Xaml::FrameworkElement const& element,
            Microsoft::UI::Xaml::Input::PointerRoutedEventArgs const& args, bool fromTray);
        void MoveDrag(Microsoft::UI::Xaml::Input::PointerRoutedEventArgs const& args);
        void EndDrag(Microsoft::UI::Xaml::Input::PointerRoutedEventArgs const& args);
        LayoutDevice const* FindDevice(hstring const& peerId) const;
        void PlaceFromMenu(Windows::Foundation::IInspectable const& sender, wchar_t const* side);

        std::vector<LayoutRect> m_localDisplays;
        std::vector<LayoutDevice> m_layoutDevices;
        std::set<std::wstring> m_onlinePeerIds;
        std::wstring m_layoutSignature;
        // Canvas pixels per logical point, and the logical point at the
        // canvas origin.
        double m_layoutScale{ 1.0 };
        double m_layoutOriginX{ 0.0 };
        double m_layoutOriginY{ 0.0 };
        std::optional<LayoutDrag> m_drag;
        bool m_layoutDirty{ false };

        // One tunable effect parameter: its key in the agent's JSON, the
        // slider that edits it and the label that shows the value.
        struct EffectSlider
        {
            std::wstring_view key;
            Microsoft::UI::Xaml::Controls::Slider slider{ nullptr };
            Microsoft::UI::Xaml::Controls::TextBlock label{ nullptr };
            std::function<std::wstring(double)> format;
        };

        void ApplyMessage(Windows::Data::Json::JsonObject const& message);
        void ApplyState(Windows::Data::Json::JsonObject const& state);
        void SetAgentLinked(bool linked);
        void ShowError(hstring const& message);

        void SetupSections();
        void SetupEffectControls();
        void ApplyEffect(Windows::Data::Json::JsonObject const& state);
        void OnEffectEdited();
        void UpdateEffectLabels();

        std::vector<EffectSlider> m_effectSliders;
        // Set while state from the agent is copied into the controls, so the
        // resulting change events are not echoed back as edits.
        bool m_applyingEffect{ false };
        bool m_applyingClipboard{ false };
        double m_effectRevision{ -1 };

        Windows::Foundation::Collections::IObservableVector<Windows::Foundation::IInspectable> m_peers{
            single_threaded_observable_vector<Windows::Foundation::IInspectable>()
        };
        std::set<std::wstring> m_connectedPeerIds;
        std::unique_ptr<::LinkCursorApp::AgentClient> m_agent;
    };
}

namespace winrt::LinkCursor::factory_implementation
{
    struct MainWindow : MainWindowT<MainWindow, implementation::MainWindow>
    {
    };
}
