// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#pragma once

#include <winrt/Rinflow.h>

// What the windows show about devices, read from the agent's state snapshot
// (see agent/src/state.rs): the user's devices, nearby ones, and the rows
// that list them.
namespace RinflowApp
{
    struct DeviceInfo
    {
        winrt::hstring id;
        winrt::hstring name;
        winrt::hstring platform;
        winrt::hstring address;
        bool online{ false };
        bool connected{ false };
        bool connecting{ false };
        // This peer drives this PC's keyboard and mouse now.
        bool controlling{ false };
        // Waiting for the users to compare the pairing code.
        bool pairing{ false };
        bool remembered{ false };
        bool placed{ false };
        // It offers a picture (a KVM: the computer behind it), for a remote
        // desktop window.
        bool picture{ false };
        std::optional<uint32_t> latencyMs;
        std::optional<uint64_t> lastSeenMs;
        // left / right / top / bottom, when it touches this PC.
        winrt::hstring side;
        // Of its main display, in pixels ("3024×1964"); empty if unknown.
        winrt::hstring resolution;
    };

    struct DeviceModel
    {
        winrt::hstring localName;
        winrt::hstring localResolution;
        // Remembered, placed or in a session: connected first, then
        // connecting, online, offline; by name within each.
        std::vector<DeviceInfo> mine;
        // Online and not (yet) one of the user's devices.
        std::vector<DeviceInfo> nearby;

        DeviceInfo const* Find(winrt::hstring const& id) const;
        static DeviceModel FromState(winrt::Windows::Data::Json::JsonObject const& state);
    };

    winrt::hstring PlatformLabel(winrt::hstring const& platform);
    winrt::hstring DeviceGlyph(winrt::hstring const& platform);
    // "3 分钟前在线", "昨天在线" …
    winrt::hstring LastSeenText(std::optional<uint64_t> unixMs);
    // The main display's pixels: the one at the origin, else the first.
    winrt::hstring ResolutionOf(winrt::Windows::Data::Json::JsonArray const& displays);

    winrt::Rinflow::PeerItem DeviceRow(DeviceInfo const& device);
    winrt::Rinflow::PeerItem NearbyRow(DeviceInfo const& device);
    winrt::Rinflow::PeerItem LocalRow(winrt::hstring const& name);
}
