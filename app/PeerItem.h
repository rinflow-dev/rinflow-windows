#pragma once

#include "PeerItem.g.h"

namespace winrt::LinkCursor::implementation
{
    struct PeerItem : PeerItemT<PeerItem>
    {
        PeerItem(hstring const& id, hstring const& name, hstring const& detail,
            hstring const& actionText, bool canAct)
            : m_id(id), m_name(name), m_detail(detail), m_actionText(actionText), m_canAct(canAct)
        {
        }

        hstring Id() const { return m_id; }
        hstring Name() const { return m_name; }
        hstring Detail() const { return m_detail; }
        hstring ActionText() const { return m_actionText; }
        bool CanAct() const { return m_canAct; }

    private:
        hstring m_id;
        hstring m_name;
        hstring m_detail;
        hstring m_actionText;
        bool m_canAct;
    };
}

namespace winrt::LinkCursor::factory_implementation
{
    struct PeerItem : PeerItemT<PeerItem, implementation::PeerItem>
    {
    };
}
