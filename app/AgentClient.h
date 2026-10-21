#pragma once

namespace LinkCursorApp
{
    // Talks to linkcursor-agent.exe over its per-user named pipe: one JSON
    // object per line in each direction. Starts the agent if it is not
    // running and reconnects if it goes away. Callbacks run on a background
    // thread; callers marshal to the UI thread themselves.
    class AgentClient
    {
    public:
        using MessageHandler = std::function<void(winrt::Windows::Data::Json::JsonObject const&)>;
        using LinkHandler = std::function<void(bool linked)>;

        AgentClient() = default;
        ~AgentClient();
        AgentClient(AgentClient const&) = delete;
        AgentClient& operator=(AgentClient const&) = delete;

        void Start(MessageHandler onMessage, LinkHandler onLink);
        void Stop();

        void Connect(std::wstring_view peerId);
        void Disconnect(std::wstring_view peerId);

        // Arrangement, in logical points of this PC's desktop space.
        void SetPlacement(std::wstring_view peerId, double x, double y);
        void RemovePlacement(std::wstring_view peerId);
        void PlaceBeside(std::wstring_view peerId, std::wstring_view side);

        void SetEffect(winrt::Windows::Data::Json::JsonObject const& params);
        void ResetEffect();
        void SetEffectDemo(bool enabled);
        void PreviewEffect();
        void SetClipboardSync(bool enabled);

        // Keys of the agent's settings (agent/src/settings.rs), plus
        // launch_at_login.
        void UpdateSettings(winrt::Windows::Data::Json::JsonObject const& values);
        void UpdateSetting(std::wstring_view key, winrt::Windows::Data::Json::IJsonValue const& value);
        // Disconnects, unplaces and stops listing a device.
        void ForgetDevice(std::wstring_view peerId);
        // Quits the agent, as the tray's 退出 does.
        void Quit();

    private:
        void Run();
        bool Open();
        void ReadLoop();
        void LaunchAgent();
        bool Send(winrt::Windows::Data::Json::JsonObject const& message);
        bool WaitOrStop(DWORD milliseconds) const;

        MessageHandler m_onMessage;
        LinkHandler m_onLink;
        std::wstring m_pipeName;
        bool m_agentLaunched{ false };
        wil::unique_event m_stop{ wil::EventOptions::ManualReset };
        std::mutex m_pipeLock;
        wil::unique_hfile m_pipe;
        std::thread m_thread;
    };
}
