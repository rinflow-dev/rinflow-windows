// The pairing dialog (the design's 配对确认). The agent reports a pairing in
// progress in its snapshot (`pairing`: the peer, the six-digit code both
// devices show, whether this side accepted); the dialog follows it. 配对
// accepts and keeps the dialog open until the other device accepts too;
// 取消 (or Esc) declines. When the pairing ends either way, the snapshot no
// longer has it and the dialog closes.

#include "pch.h"
#include "MainWindow.xaml.h"
#include "Strings.h"

// Fully qualified: WIL declares a global ::Microsoft namespace as well.
using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;
using namespace winrt::Microsoft::UI::Xaml::Controls;
using namespace winrt::Windows::Data::Json;

namespace winrt::LinkCursor::implementation
{
    void MainWindow::SetupPairing()
    {
        auto weak = get_weak();
        PairingDialog().PrimaryButtonClick([weak](auto&&, ContentDialogButtonClickEventArgs const& args)
        {
            auto self = weak.get();
            if (!self) return;
            // Stays open until the other device accepts too.
            args.Cancel(true);
            if (!self->m_pairingPeer.empty())
            {
                self->m_agent->AcceptPairing(self->m_pairingPeer);
            }
            self->PairingDialog().IsPrimaryButtonEnabled(false);
            self->PairingStatus().Text(LinkCursorApp::Text(L"PairingWaiting"));
        });
        PairingDialog().Closed([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self) return;
            self->m_pairingOpen = false;
            // Closed by the user: decline. Closed because the pairing ended
            // (paired, declined there, timed out): nothing more to say.
            if (!self->m_pairingClosing && !self->m_pairingPeer.empty())
            {
                self->m_agent->RejectPairing(self->m_pairingPeer);
            }
            self->m_pairingClosing = false;
            self->m_pairingPeer = L"";
        });

        m_pairingRetry = DispatcherQueue().CreateTimer();
        m_pairingRetry.Interval(std::chrono::milliseconds(400));
        m_pairingRetry.IsRepeating(false);
        m_pairingRetry.Tick([weak](auto&& timer, auto&&)
        {
            timer.Stop();
            if (auto self = weak.get(); self && !self->m_closed && self->m_lastState) self->ApplyPairing(self->m_lastState);
        });
    }

    void MainWindow::ApplyPairing(JsonObject const& state)
    {
        auto value = state.GetNamedValue(L"pairing", JsonValue::CreateNullValue());
        if (value.ValueType() != JsonValueType::Object)
        {
            if (m_pairingOpen)
            {
                m_pairingClosing = true;
                PairingDialog().Hide();
            }
            return;
        }
        auto pairing = value.GetObject();
        auto peerId = pairing.GetNamedString(L"peer_id", L"");
        bool accepted = pairing.GetNamedBoolean(L"accepted", false);
        std::wstring code{ pairing.GetNamedString(L"code", L"") };
        if (code.size() == 6)
        {
            code.insert(3, L" ");
        }
        PairingDialog().Title(box_value(LinkCursorApp::Format(L"PairingTitle", std::wstring(pairing.GetNamedString(L"name", peerId)))));
        PairingCode().Text(code);
        PairingDialog().IsPrimaryButtonEnabled(!accepted);
        PairingStatus().Text(LinkCursorApp::Text(accepted ? L"PairingWaiting" : L"PairingInstruction"));
        if (m_pairingOpen)
        {
            m_pairingPeer = peerId;
            return;
        }

        // One dialog at a time: another one gives way first, and this one
        // shows once it has closed (or the window is ready).
        if (m_addDeviceOpen)
        {
            AddDeviceDialog().Hide();
        }
        HotkeyDialog().Hide();
        if (!Root().XamlRoot())
        {
            m_pairingRetry.Start();
            return;
        }
        m_pairingPeer = peerId;
        m_pairingOpen = true;
        m_pairingClosing = false;
        auto failed = [weak = get_weak()]
        {
            auto self = weak.get();
            if (!self || self->m_closed) return;
            self->m_pairingOpen = false;
            // Never opened, so no Closed event resets it: a pairing that
            // ended meanwhile must not make the next 取消 skip declining.
            self->m_pairingClosing = false;
            self->m_pairingPeer = L"";
            self->m_pairingRetry.Start();
        };
        try
        {
            PairingDialog().XamlRoot(Root().XamlRoot());
            // Fails while another dialog is still closing; try again shortly.
            PairingDialog().ShowAsync().Completed([failed](auto const&, Windows::Foundation::AsyncStatus status)
            {
                if (status == Windows::Foundation::AsyncStatus::Error) failed();
            });
        }
        catch (hresult_error const&)
        {
            failed();
        }
    }
}
