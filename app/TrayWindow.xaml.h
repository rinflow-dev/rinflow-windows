#pragma once

#include "TrayWindow.g.h"
#include "AgentClient.h"

namespace winrt::LinkCursor::implementation
{
    struct TrayWindow : TrayWindowT<TrayWindow>
    {
        TrayWindow()
        {
            // Xaml objects should not call InitializeComponent during construction.
        }

        void InitializeComponent();

        Windows::Foundation::Collections::IObservableVector<Windows::Foundation::IInspectable> Devices() const
        {
            return m_devices;
        }

        // Places the panel by the notification area, next to the pointer.
        void ShowNearPointer();

        // Closes the panel once, after the current message: closing a
        // window from inside its own activation change (it loses focus while
        // another window activates) tears XAML down under its feet.
        void Dismiss();

    private:
        void ApplyState(Windows::Data::Json::JsonObject const& state);
        void FitToContent();

        std::unique_ptr<::LinkCursorApp::AgentClient> m_agent;
        Windows::Foundation::Collections::IObservableVector<Windows::Foundation::IInspectable> m_devices{
            single_threaded_observable_vector<Windows::Foundation::IInspectable>()
        };
        bool m_applying{ false };
        bool m_wasActive{ false };
        bool m_closing{ false };
        hstring m_theme;
        // Where the pointer was when the panel opened, in physical pixels.
        POINT m_anchor{};
        double m_scale{ 1.0 };
    };
}

namespace winrt::LinkCursor::factory_implementation
{
    struct TrayWindow : TrayWindowT<TrayWindow, implementation::TrayWindow>
    {
    };
}
