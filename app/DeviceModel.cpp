// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#include "pch.h"
#include "DeviceModel.h"
#include "PeerItem.h"
#include "Strings.h"

using namespace winrt;
using namespace winrt::Windows::Data::Json;

namespace
{
    std::wstring Lower(hstring const& text)
    {
        std::wstring lower{ text };
        for (auto& c : lower) c = static_cast<wchar_t>(towlower(c));
        return lower;
    }

    int Rank(RinflowApp::DeviceInfo const& device)
    {
        if (device.connected) return 0;
        if (device.connecting) return 1;
        if (device.online) return 2;
        return 3;
    }

    uint64_t NowMs()
    {
        FILETIME now{};
        GetSystemTimeAsFileTime(&now);
        ULARGE_INTEGER value{ { now.dwLowDateTime, now.dwHighDateTime } };
        // 100 ns ticks since 1601 to milliseconds since 1970.
        return (value.QuadPart - 116444736000000000ULL) / 10000;
    }

    std::tm LocalTime(uint64_t unixMs)
    {
        __time64_t seconds = static_cast<__time64_t>(unixMs / 1000);
        std::tm local{};
        _localtime64_s(&local, &seconds);
        return local;
    }
}

namespace RinflowApp
{
    hstring PlatformLabel(hstring const& platform)
    {
        if (platform == L"macos") return L"macOS";
        if (platform == L"windows") return L"Windows";
        if (platform == L"linux") return L"Linux";
        if (platform == L"android") return L"Android";
        return Text(L"UnknownSystem");
    }

    hstring DeviceGlyph(hstring const& platform)
    {
        if (platform == L"macos") return L"\xE7F8";   // laptop
        if (platform == L"android") return L"\xE8EA"; // phone
        return L"\xE7F4";                             // monitor
    }

    hstring LastSeenText(std::optional<uint64_t> unixMs)
    {
        if (!unixMs || *unixMs == 0)
        {
            return Text(L"Offline");
        }
        auto now = NowMs();
        auto minutes = now > *unixMs ? (now - *unixMs) / 60000 : 0;
        if (minutes < 1) return Text(L"OnlineJustNow");
        if (minutes < 60) return Format(L"OnlineMinutesAgo", minutes);
        auto then = LocalTime(*unixMs);
        auto today = LocalTime(now);
        auto yesterday = LocalTime(now - 86400000ULL);
        auto sameDay = [](std::tm const& a, std::tm const& b) { return a.tm_year == b.tm_year && a.tm_yday == b.tm_yday; };
        if (sameDay(then, today)) return Format(L"OnlineTodayAt", then.tm_hour, then.tm_min);
        if (sameDay(then, yesterday)) return Text(L"OnlineYesterday");
        return Format(L"OnlineOnDate", then.tm_mon + 1, then.tm_mday);
    }

    hstring ResolutionOf(JsonArray const& displays)
    {
        if (displays.Size() == 0)
        {
            return L"";
        }
        auto chosen = displays.GetObjectAt(0);
        for (auto const& value : displays)
        {
            auto display = value.GetObject();
            if (display.GetNamedNumber(L"x", 1) == 0 && display.GetNamedNumber(L"y", 1) == 0)
            {
                chosen = display;
                break;
            }
        }
        auto scale = chosen.GetNamedNumber(L"scale", 1);
        auto width = std::lround(chosen.GetNamedNumber(L"width", 0) * scale);
        auto height = std::lround(chosen.GetNamedNumber(L"height", 0) * scale);
        if (width <= 0 || height <= 0)
        {
            return L"";
        }
        return hstring(std::format(L"{}×{}", width, height));
    }

    DeviceInfo const* DeviceModel::Find(hstring const& id) const
    {
        for (auto const* list : { &mine, &nearby })
        {
            for (auto const& device : *list)
            {
                if (device.id == id) return &device;
            }
        }
        return nullptr;
    }

    DeviceModel DeviceModel::FromState(JsonObject const& state)
    {
        DeviceModel model;
        model.localName = state.GetNamedString(L"device_name", L"Rinflow");
        auto layout = state.GetNamedObject(L"layout", JsonObject{});
        model.localResolution = ResolutionOf(layout.GetNamedArray(L"displays", JsonArray{}));

        std::map<std::wstring, DeviceInfo> devices;
        auto entry = [&](hstring const& id) -> DeviceInfo&
        {
            auto& device = devices[std::wstring(id)];
            if (device.id.empty())
            {
                device.id = id;
                device.name = id;
            }
            return device;
        };

        // Every peer the agent knows (seen at some point), with its displays.
        for (auto const& value : layout.GetNamedArray(L"devices", JsonArray{}))
        {
            auto view = value.GetObject();
            auto& device = entry(view.GetNamedString(L"peer_id", L""));
            device.name = view.GetNamedString(L"name", device.id);
            device.platform = view.GetNamedString(L"platform", L"");
            device.remembered = view.GetNamedBoolean(L"remembered", false);
            device.placed = view.GetNamedValue(L"offset", JsonValue::CreateNullValue()).ValueType() == JsonValueType::Object;
            auto side = view.GetNamedValue(L"side", JsonValue::CreateNullValue());
            if (side.ValueType() == JsonValueType::String) device.side = side.GetString();
            auto lastSeen = view.GetNamedValue(L"last_seen_ms", JsonValue::CreateNullValue());
            if (lastSeen.ValueType() == JsonValueType::Number) device.lastSeenMs = static_cast<uint64_t>(lastSeen.GetNumber());
            device.resolution = ResolutionOf(view.GetNamedArray(L"displays", JsonArray{}));
        }
        // Online now.
        for (auto const& value : state.GetNamedArray(L"peers", JsonArray{}))
        {
            auto peer = value.GetObject();
            auto& device = entry(peer.GetNamedString(L"id", L""));
            device.online = true;
            device.name = peer.GetNamedString(L"name", device.name);
            auto platform = peer.GetNamedString(L"platform", L"");
            if (!platform.empty()) device.platform = platform;
            device.address = peer.GetNamedString(L"address", L"");
        }
        // Sessions.
        for (auto const& value : state.GetNamedArray(L"sessions", JsonArray{}))
        {
            auto session = value.GetObject();
            auto& device = entry(session.GetNamedString(L"peer_id", L""));
            auto name = session.GetNamedString(L"name", L"");
            if (!name.empty() && name != device.id) device.name = name;
            if (session.GetNamedString(L"status", L"") == L"connected")
            {
                device.connected = true;
                device.online = true;
                device.controlling = session.GetNamedBoolean(L"remote_control", false);
                auto latency = session.GetNamedValue(L"latency_ms", JsonValue::CreateNullValue());
                if (latency.ValueType() == JsonValueType::Number) device.latencyMs = static_cast<uint32_t>(latency.GetNumber());
            }
            else
            {
                device.connecting = true;
            }
        }
        if (auto pairing = state.GetNamedValue(L"pairing", JsonValue::CreateNullValue()); pairing.ValueType() == JsonValueType::Object)
        {
            auto view = pairing.GetObject();
            auto& device = entry(view.GetNamedString(L"peer_id", L""));
            if (device.name == device.id) device.name = view.GetNamedString(L"name", device.name);
            if (device.platform.empty()) device.platform = view.GetNamedString(L"platform", L"");
            device.pairing = true;
            device.connecting = !device.connected;
        }

        for (auto& [id, device] : devices)
        {
            if (device.id.empty()) continue;
            if (device.remembered || device.placed || device.connected || device.connecting)
            {
                model.mine.push_back(std::move(device));
            }
            else if (device.online)
            {
                model.nearby.push_back(std::move(device));
            }
        }
        auto order = [](DeviceInfo const& a, DeviceInfo const& b)
        {
            auto ra = Rank(a), rb = Rank(b);
            if (ra != rb) return ra < rb;
            return Lower(a.name) < Lower(b.name);
        };
        std::sort(model.mine.begin(), model.mine.end(), order);
        std::sort(model.nearby.begin(), model.nearby.end(), order);
        return model;
    }

    namespace
    {
        hstring Detail(DeviceInfo const& device)
        {
            auto platform = PlatformLabel(device.platform);
            if (device.online)
            {
                return device.address.empty() ? platform : platform + L" · " + device.address;
            }
            return platform + L" · " + LastSeenText(device.lastSeenMs);
        }
    }

    winrt::Rinflow::PeerItem DeviceRow(DeviceInfo const& device)
    {
        auto row = make_self<winrt::Rinflow::implementation::PeerItem>(
            device.id, device.name, Detail(device), DeviceGlyph(device.platform));
        if (device.connected)
        {
            row->IsConnected(true);
            row->StatusText(Text(device.controlling ? L"StatusControllingThis" : L"StatusConnected"));
            row->Latency(!device.latencyMs ? hstring(L"…")
                : *device.latencyMs == 0 ? hstring(L"<1 ms")
                : hstring(std::format(L"{} ms", *device.latencyMs)));
        }
        else if (device.pairing)
        {
            row->StatusText(Text(L"StatusPairing"));
        }
        else if (device.connecting)
        {
            row->StatusText(Text(L"StatusConnecting"));
        }
        else if (device.online)
        {
            row->StatusText(Text(L"StatusNotConnected"));
            row->CanConnect(true);
        }
        else
        {
            row->StatusText(Text(L"Offline"));
            row->Latency(L"—");
            row->IsOffline(true);
        }
        return *row;
    }

    winrt::Rinflow::PeerItem NearbyRow(DeviceInfo const& device)
    {
        auto row = make_self<winrt::Rinflow::implementation::PeerItem>(
            device.id, device.name, Detail(device), DeviceGlyph(device.platform));
        row->CanConnect(!device.connecting);
        row->StatusText(device.pairing ? Text(L"StatusPairing") : device.connecting ? Text(L"StatusConnecting") : hstring());
        return *row;
    }

    winrt::Rinflow::PeerItem LocalRow(hstring const& name)
    {
        auto row = make_self<winrt::Rinflow::implementation::PeerItem>(
            L"", name, Text(L"ThisPC"), DeviceGlyph(L"windows"));
        row->StatusText(Text(L"ThisPC"));
        return *row;
    }
}
