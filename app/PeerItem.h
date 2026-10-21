#pragma once

#include "PeerItem.g.h"

namespace winrt::LinkCursor::implementation
{
    struct PeerItem : PeerItemT<PeerItem>
    {
        using Visibility = Microsoft::UI::Xaml::Visibility;

        PeerItem(hstring const& id, hstring const& name, hstring const& detail, hstring const& glyph)
            : m_id(id), m_name(name), m_detail(detail), m_glyph(glyph)
        {
        }

        hstring Id() const { return m_id; }
        hstring Name() const { return m_name; }
        hstring Detail() const { return m_detail; }
        hstring Glyph() const { return m_glyph; }

        hstring Latency() const { return m_latency; }
        void Latency(hstring const& value) { m_latency = value; }
        hstring StatusText() const { return m_statusText; }
        void StatusText(hstring const& value) { m_statusText = value; }
        bool IsConnected() const { return m_connected; }
        void IsConnected(bool value) { m_connected = value; }
        bool IsOffline() const { return m_offline; }
        void IsOffline(bool value) { m_offline = value; }
        bool CanConnect() const { return m_canConnect; }
        void CanConnect(bool value) { m_canConnect = value; }

        Visibility ConnectedVisibility() const { return Show(m_connected); }
        Visibility OtherVisibility() const { return Show(!m_connected); }
        Visibility LatencyVisibility() const { return Show(!m_canConnect && !m_latency.empty()); }
        Visibility ConnectVisibility() const { return Show(m_canConnect); }
        double ContentOpacity() const { return m_offline ? 0.55 : 1.0; }

    private:
        static Visibility Show(bool visible) { return visible ? Visibility::Visible : Visibility::Collapsed; }

        hstring m_id;
        hstring m_name;
        hstring m_detail;
        hstring m_glyph;
        hstring m_latency;
        hstring m_statusText;
        bool m_connected{ false };
        bool m_offline{ false };
        bool m_canConnect{ false };
    };
}

namespace winrt::LinkCursor::factory_implementation
{
    struct PeerItem : PeerItemT<PeerItem, implementation::PeerItem>
    {
    };
}
