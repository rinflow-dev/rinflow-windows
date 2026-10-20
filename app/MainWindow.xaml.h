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

    private:
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
