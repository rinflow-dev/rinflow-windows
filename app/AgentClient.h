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
        // Where SetPlacement would put it, answered with "placement_preview".
        void PreviewPlacement(std::wstring_view peerId, double x, double y);
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
        // Quits the agent, as the tray's 退出 does; it is not started again.
        void Quit();
        // The user dismissed the agent's last_error.
        void ClearError();
        // Stops files being received or sent (the snapshot's transfers).
        void CancelTransfer(uint32_t transfer);
        void AcceptTransfer(uint32_t transfer);

        // The user compared the pairing code and accepts, or declines.
        void AcceptPairing(std::wstring_view peerId);
        void RejectPairing(std::wstring_view peerId);

    private:
        void Run();
        bool Open();
        void ReadLoop();
        void LaunchAgent();
        static bool IsAgentPipe(HANDLE pipe);
        bool Send(winrt::Windows::Data::Json::JsonObject const& message);
        bool WaitOrStop(DWORD milliseconds) const;

        MessageHandler m_onMessage;
        LinkHandler m_onLink;
        std::wstring m_pipeName;
        // Started once per absence of the agent: again only after it had
        // answered and went away (a crash), never after the user quit it.
        bool m_agentLaunched{ false };
        std::atomic<bool> m_agentQuit{ false };
        wil::unique_event m_stop{ wil::EventOptions::ManualReset };
        std::mutex m_pipeLock;
        wil::unique_hfile m_pipe;
        std::thread m_thread;
    };
}
