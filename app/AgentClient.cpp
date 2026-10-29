#include "pch.h"
#include "AgentClient.h"

using namespace winrt::Windows::Data::Json;

namespace
{
    // Must match PROTOCOL_VERSION in agent/src/ipc.rs.
    constexpr int ProtocolVersion = 14;

    // A started agent opens its pipe within a second or two; past this it
    // is stuck (or another agent exited 0 for one that never came up).
    constexpr auto AgentAnswerTimeout = std::chrono::seconds(15);

    // Must match pipe_name() in agent/src/ipc.rs.
    std::wstring AgentPipeName()
    {
        auto user = wil::get_token_information<TOKEN_USER>();
        wil::unique_hlocal_string sid;
        THROW_IF_WIN32_BOOL_FALSE(ConvertSidToStringSidW(user->User.Sid, sid.put()));
        return std::wstring(L"\\\\.\\pipe\\LinkCursor.Agent.") + sid.get();
    }

    // LINKCURSOR_AGENT_PATH, or linkcursor-agent.exe next to this executable.
    std::wstring AgentExecutablePath()
    {
        if (auto overridePath = wil::TryGetEnvironmentVariableW(L"LINKCURSOR_AGENT_PATH"))
        {
            return overridePath.get();
        }
        auto exe = wil::GetModuleFileNameW<wil::unique_cotaskmem_string>(nullptr);
        std::wstring path = exe.get();
        path.resize(path.find_last_of(L'\\') + 1);
        return path + L"linkcursor-agent.exe";
    }

    JsonObject Request(std::wstring_view type)
    {
        JsonObject message;
        message.Insert(L"type", JsonValue::CreateStringValue(type));
        return message;
    }
}

namespace LinkCursorApp
{
    AgentClient::~AgentClient()
    {
        Stop();
    }

    void AgentClient::Start(MessageHandler onMessage, LinkHandler onLink, ProblemHandler onProblem)
    {
        m_onMessage = std::move(onMessage);
        m_onLink = std::move(onLink);
        m_onProblem = std::move(onProblem);
        m_pipeName = AgentPipeName();
        m_thread = std::thread([this] { Run(); });
    }

    void AgentClient::Retry()
    {
        m_retry = true;
    }

    void AgentClient::Stop()
    {
        m_stop.SetEvent();
        if (m_thread.joinable())
        {
            m_thread.join();
        }
    }

    void AgentClient::Connect(std::wstring_view peerId)
    {
        auto message = Request(L"connect");
        message.Insert(L"peer_id", JsonValue::CreateStringValue(peerId));
        Send(message);
    }

    void AgentClient::Disconnect(std::wstring_view peerId)
    {
        auto message = Request(L"disconnect");
        message.Insert(L"peer_id", JsonValue::CreateStringValue(peerId));
        Send(message);
    }

    void AgentClient::SetPlacement(std::wstring_view peerId, double x, double y)
    {
        auto message = Request(L"set_placement");
        message.Insert(L"peer_id", JsonValue::CreateStringValue(peerId));
        message.Insert(L"x", JsonValue::CreateNumberValue(x));
        message.Insert(L"y", JsonValue::CreateNumberValue(y));
        Send(message);
    }

    void AgentClient::PreviewPlacement(std::wstring_view peerId, double x, double y)
    {
        auto message = Request(L"preview_placement");
        message.Insert(L"peer_id", JsonValue::CreateStringValue(peerId));
        message.Insert(L"x", JsonValue::CreateNumberValue(x));
        message.Insert(L"y", JsonValue::CreateNumberValue(y));
        Send(message);
    }

    void AgentClient::RemovePlacement(std::wstring_view peerId)
    {
        auto message = Request(L"remove_placement");
        message.Insert(L"peer_id", JsonValue::CreateStringValue(peerId));
        Send(message);
    }

    void AgentClient::PlaceBeside(std::wstring_view peerId, std::wstring_view side)
    {
        auto message = Request(L"place_beside");
        message.Insert(L"peer_id", JsonValue::CreateStringValue(peerId));
        message.Insert(L"side", JsonValue::CreateStringValue(side));
        Send(message);
    }

    void AgentClient::SetEffect(JsonObject const& params)
    {
        auto message = Request(L"set_effect");
        message.Insert(L"params", params);
        Send(message);
    }

    void AgentClient::ResetEffect()
    {
        Send(Request(L"reset_effect"));
    }

    void AgentClient::SetEffectDemo(bool enabled)
    {
        auto message = Request(L"set_effect_demo");
        message.Insert(L"enabled", JsonValue::CreateBooleanValue(enabled));
        Send(message);
    }

    void AgentClient::SetClipboardSync(bool enabled)
    {
        auto message = Request(L"set_clipboard_sync");
        message.Insert(L"enabled", JsonValue::CreateBooleanValue(enabled));
        Send(message);
    }

    void AgentClient::PreviewEffect()
    {
        Send(Request(L"effect_preview"));
    }

    void AgentClient::UpdateSettings(JsonObject const& values)
    {
        auto message = Request(L"update_settings");
        message.Insert(L"values", values);
        Send(message);
    }

    void AgentClient::UpdateSetting(std::wstring_view key, IJsonValue const& value)
    {
        JsonObject values;
        values.Insert(key, value);
        UpdateSettings(values);
    }

    void AgentClient::ForgetDevice(std::wstring_view peerId)
    {
        auto message = Request(L"forget_device");
        message.Insert(L"peer_id", JsonValue::CreateStringValue(peerId));
        Send(message);
    }

    void AgentClient::Quit()
    {
        m_agentQuit = true;
        Send(Request(L"quit"));
    }

    void AgentClient::ClearError()
    {
        Send(Request(L"clear_error"));
    }

    void AgentClient::CancelTransfer(uint32_t transfer)
    {
        auto message = Request(L"cancel_transfer");
        message.Insert(L"transfer", JsonValue::CreateNumberValue(transfer));
        Send(message);
    }

    void AgentClient::AcceptTransfer(uint32_t transfer)
    {
        auto message = Request(L"accept_transfer");
        message.Insert(L"transfer", JsonValue::CreateNumberValue(transfer));
        Send(message);
    }

    void AgentClient::AcceptPairing(std::wstring_view peerId)
    {
        auto message = Request(L"accept_pairing");
        message.Insert(L"peer_id", JsonValue::CreateStringValue(peerId));
        Send(message);
    }

    void AgentClient::RejectPairing(std::wstring_view peerId)
    {
        auto message = Request(L"reject_pairing");
        message.Insert(L"peer_id", JsonValue::CreateStringValue(peerId));
        Send(message);
    }

    void AgentClient::Run()
    {
        while (!m_stop.is_signaled())
        {
            if (!Open())
            {
                WaitOrStop(250);
                continue;
            }

            auto hello = Request(L"hello");
            hello.Insert(L"protocol", JsonValue::CreateNumberValue(ProtocolVersion));
            Send(hello);
            m_onLink(true);

            ReadLoop();

            {
                std::lock_guard lock(m_pipeLock);
                m_pipe.reset();
            }
            m_onLink(false);
        }
    }

    bool AgentClient::Open()
    {
        if (m_retry.exchange(false))
        {
            // Start it afresh, even after the user had quit it: they asked.
            m_agentLaunched = false;
            m_agentQuit = false;
            m_agentProcess.reset();
            m_problem = AgentProblem::None;
        }

        // Overlapped I/O lets the reader thread block in ReadFile while the
        // UI thread writes; synchronous handles would serialize the two. The
        // server may identify this user but not act as it.
        HANDLE pipe = CreateFileW(m_pipeName.c_str(), GENERIC_READ | GENERIC_WRITE, 0, nullptr,
            OPEN_EXISTING, FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION, nullptr);
        if (pipe != INVALID_HANDLE_VALUE)
        {
            wil::unique_hfile opened{ pipe };
            if (!IsAgentPipe(opened.get()))
            {
                OutputDebugStringW(L"LinkCursor: the agent pipe belongs to another user; not using it\n");
                WaitOrStop(2000);
                return false;
            }
            {
                std::lock_guard lock(m_pipeLock);
                m_pipe = std::move(opened);
            }
            // Answered: should it go away later (not quit), start it again.
            m_agentLaunched = false;
            m_agentProcess.reset();
            Report(AgentProblem::None, 0);
            return true;
        }

        switch (GetLastError())
        {
        case ERROR_PIPE_BUSY:
            WaitNamedPipeW(m_pipeName.c_str(), 1000);
            break;
        case ERROR_FILE_NOT_FOUND:
            // Launch once; after that keep retrying while the agent starts
            // or while the user restarts it from the tray.
            if (!m_agentLaunched && !m_agentQuit)
            {
                m_agentLaunched = true;
                LaunchAgent();
            }
            else if (m_agentProcess)
            {
                CheckLaunch();
            }
            break;
        }
        return false;
    }

    bool AgentClient::IsAgentPipe(HANDLE pipe)
    {
        // The agent creates the pipe as this user (or, run as administrator,
        // owned by Administrators); a pipe another user created first is not
        // the agent.
        PSID owner = nullptr;
        wil::unique_hlocal_security_descriptor descriptor;
        if (GetSecurityInfo(pipe, SE_KERNEL_OBJECT, OWNER_SECURITY_INFORMATION, &owner, nullptr, nullptr, nullptr,
                descriptor.put()) != ERROR_SUCCESS || !owner)
        {
            return false;
        }
        auto user = wil::get_token_information<TOKEN_USER>();
        return EqualSid(owner, user->User.Sid) || IsWellKnownSid(owner, WinBuiltinAdministratorsSid);
    }

    void AgentClient::LaunchAgent()
    {
        auto path = AgentExecutablePath();
        STARTUPINFOW startup{ sizeof(startup) };
        PROCESS_INFORMATION process{};
        m_launchedAt = std::chrono::steady_clock::now();
        if (!CreateProcessW(path.c_str(), nullptr, nullptr, nullptr, FALSE, CREATE_NO_WINDOW,
            nullptr, nullptr, &startup, &process))
        {
            auto error = GetLastError();
            OutputDebugStringW((L"LinkCursor: cannot start agent at " + path + L"\n").c_str());
            Report(AgentProblem::CannotStart, error);
            return;
        }
        CloseHandle(process.hThread);
        m_agentProcess.reset(process.hProcess);
    }

    void AgentClient::CheckLaunch()
    {
        if (WaitForSingleObject(m_agentProcess.get(), 0) == WAIT_OBJECT_0)
        {
            DWORD exitCode = 0;
            GetExitCodeProcess(m_agentProcess.get(), &exitCode);
            // 0: another agent of this session was running already. Its
            // pipe is about to appear, or the timeout below says it did not.
            if (exitCode != 0)
            {
                m_agentProcess.reset();
                Report(AgentProblem::Exited, exitCode);
                return;
            }
        }
        if (std::chrono::steady_clock::now() - m_launchedAt > AgentAnswerTimeout)
        {
            m_agentProcess.reset();
            Report(AgentProblem::NoAnswer, 0);
        }
    }

    void AgentClient::Report(AgentProblem problem, uint32_t code)
    {
        if (problem == m_problem && problem == AgentProblem::None)
        {
            return;
        }
        m_problem = problem;
        if (m_onProblem)
        {
            m_onProblem(problem, code);
        }
    }

    void AgentClient::ReadLoop()
    {
        wil::unique_event readDone{ wil::EventOptions::ManualReset };
        std::string buffer;
        char chunk[4096];

        for (;;)
        {
            OVERLAPPED overlapped{};
            overlapped.hEvent = readDone.get();
            readDone.ResetEvent();
            if (!ReadFile(m_pipe.get(), chunk, sizeof(chunk), nullptr, &overlapped))
            {
                if (GetLastError() != ERROR_IO_PENDING)
                {
                    return;
                }
                HANDLE waits[] = { readDone.get(), m_stop.get() };
                if (WaitForMultipleObjects(2, waits, FALSE, INFINITE) != WAIT_OBJECT_0)
                {
                    DWORD ignored = 0;
                    CancelIoEx(m_pipe.get(), &overlapped);
                    GetOverlappedResult(m_pipe.get(), &overlapped, &ignored, TRUE);
                    return;
                }
            }

            DWORD read = 0;
            if (!GetOverlappedResult(m_pipe.get(), &overlapped, &read, FALSE) || read == 0)
            {
                return;
            }

            buffer.append(chunk, read);
            for (auto newline = buffer.find('\n'); newline != std::string::npos; newline = buffer.find('\n'))
            {
                JsonObject message{ nullptr };
                if (JsonObject::TryParse(winrt::to_hstring(std::string_view(buffer).substr(0, newline)), message))
                {
                    // The user quit LinkCursor: the agent is not started
                    // again when its pipe closes in a moment.
                    if (message.GetNamedBoolean(L"quitting", false))
                    {
                        m_agentQuit = true;
                    }
                    m_onMessage(message);
                }
                buffer.erase(0, newline + 1);
            }
        }
    }

    bool AgentClient::Send(JsonObject const& message)
    {
        auto line = winrt::to_string(message.Stringify());
        line.push_back('\n');

        std::lock_guard lock(m_pipeLock);
        if (!m_pipe)
        {
            return false;
        }
        wil::unique_event writeDone{ wil::EventOptions::ManualReset };
        OVERLAPPED overlapped{};
        overlapped.hEvent = writeDone.get();
        if (!WriteFile(m_pipe.get(), line.data(), static_cast<DWORD>(line.size()), nullptr, &overlapped)
            && GetLastError() != ERROR_IO_PENDING)
        {
            return false;
        }
        DWORD written = 0;
        return GetOverlappedResult(m_pipe.get(), &overlapped, &written, TRUE)
            && written == line.size();
    }

    bool AgentClient::WaitOrStop(DWORD milliseconds) const
    {
        return WaitForSingleObject(m_stop.get(), milliseconds) == WAIT_OBJECT_0;
    }
}
