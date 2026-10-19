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
        void Disconnect();

        void SetEffect(winrt::Windows::Data::Json::JsonObject const& params);
        void ResetEffect();
        void SetEffectDemo(bool enabled);
        void PreviewEffect();

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
