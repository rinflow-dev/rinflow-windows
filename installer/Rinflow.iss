; Rinflow for Windows: the setup program (Inno Setup 6).
;
; Built by build-installer.ps1, which builds the Release app first and
; passes the version and the folder to package:
;   ISCC.exe /DAppVersion=0.1.0 /DSourceDir=..\app\x64\Release\Rinflow Rinflow.iss
;
; What it installs, into Program Files\Rinflow (only administrators can
; change files there, which the input helper and the agent rely on when they
; check each other):
; - Rinflow.exe with the Windows App Runtime it needs (self-contained),
;   rinflow-agent.exe, rinflow-input.exe and rinflow-service.exe.
; - A firewall rule for the agent, on private and public networks: Windows
;   often marks a new Wi-Fi network public, and Rinflow would then find
;   no device without saying why. (This, and Program Files, are why setup
;   needs an administrator even without the service.)
; - HKLM\SOFTWARE\Rinflow ElevatedInput: whether devices controlling this
;   PC may operate elevated windows, UAC prompts and the lock screen. A task
;   on the first install, checked by default; the settings switch changes it
;   later, and an upgrade keeps what is set.
; - The RinflowInput service (rinflow-service.exe --install), a
;   LocalSystem process, only while ElevatedInput is on: the settings switch
;   installs it when turned on and removes it when turned off.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\app\x64\Release\Rinflow"
#endif

[Setup]
AppId={{6B0E5C1A-3F4D-4E8B-9A27-5C1D2E8F4A90}
AppName=Rinflow
AppVersion={#AppVersion}
AppVerName=Rinflow {#AppVersion}
AppPublisher=Rinflow
AppPublisherURL=https://github.com/rinflow-dev/rinflow-windows
DefaultDirName={autopf}\Rinflow
DisableProgramGroupPage=yes
; Always Program Files: the service runs as LocalSystem and trusts the
; programs beside it (and logs there), so their folder must be one only
; administrators can change. A folder picked elsewhere inherits write access
; for every user from the drive root.
DisableDirPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir=output
OutputBaseFilename=Rinflow-Setup-{#AppVersion}-x64
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
SetupIconFile=..\app\Assets\Rinflow.Shell.ico
; Closes Rinflow when it holds files being replaced.
CloseApplications=force
RestartApplications=no
UninstallDisplayIcon={app}\Rinflow.exe
UninstallDisplayName=Rinflow
SetupLogging=yes
; Sign setup and uninstaller once there is a certificate, e.g.
; SignTool=signtool sign /fd sha256 /tr http://timestamp.digicert.com /td sha256 $f

; English, Chinese or Japanese, from the Windows display language like the
; app; setup asks only when none matches.
ShowLanguageDialog=auto
LanguageDetectionMethod=uilanguage

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimp"; MessagesFile: "ChineseSimplified.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"

[CustomMessages]
en.GroupControlled=While another device controls this PC:
en.TaskElevatedInput=Allow it to operate administrator windows, UAC prompts and the lock screen (recommended)
en.GroupStartup=Startup:
en.TaskAutostart=Start Rinflow when you sign in to Windows
en.GroupShortcuts=Shortcuts:
en.TaskDesktopIcon=Create a desktop shortcut
en.StatusFirewall=Setting up the firewall…
en.StatusService=Installing the input service…
en.RunApp=Start Rinflow
chinesesimp.GroupControlled=被控制时：
chinesesimp.TaskElevatedInput=被其他设备控制时，也能操作管理员窗口、UAC 弹窗和锁屏（推荐）
chinesesimp.GroupStartup=启动：
chinesesimp.TaskAutostart=登录 Windows 时启动 Rinflow
chinesesimp.GroupShortcuts=快捷方式：
chinesesimp.TaskDesktopIcon=创建桌面快捷方式
chinesesimp.StatusFirewall=正在设置防火墙…
chinesesimp.StatusService=正在安装输入服务…
chinesesimp.RunApp=启动 Rinflow
japanese.GroupControlled=他のデバイスがこの PC を操作しているとき:
japanese.TaskElevatedInput=管理者ウィンドウ、UAC の確認画面、ロック画面も操作できるようにする (推奨)
japanese.GroupStartup=起動:
japanese.TaskAutostart=Windows へのサインイン時に Rinflow を起動する
japanese.GroupShortcuts=ショートカット:
japanese.TaskDesktopIcon=デスクトップにショートカットを作成する
japanese.StatusFirewall=ファイアウォールを設定しています…
japanese.StatusService=入力サービスをインストールしています…
japanese.RunApp=Rinflow を起動する

[Tasks]
; Asked on the first install only: later the settings switch decides, and
; an upgrade keeps it (ElevatedChosen).
Name: "elevatedinput"; Description: "{cm:TaskElevatedInput}"; GroupDescription: "{cm:GroupControlled}"; Check: IsFirstInstall
Name: "autostart"; Description: "{cm:TaskAutostart}"; GroupDescription: "{cm:GroupStartup}"
Name: "desktopicon"; Description: "{cm:TaskDesktopIcon}"; GroupDescription: "{cm:GroupShortcuts}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Excludes: "*.pdb,*.old,*.ilk,*.exp,*.lib,Microsoft.Web.WebView2.*"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\THIRD_PARTY_NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; An unpackaged app's shortcuts cannot follow the theme, so they use the EXE's
; plated icon, which is legible on both.
Name: "{autoprograms}\Rinflow"; Filename: "{app}\Rinflow.exe"
Name: "{autodesktop}\Rinflow"; Filename: "{app}\Rinflow.exe"; Tasks: desktopicon

[Registry]
Root: HKLM64; Subkey: "SOFTWARE\Rinflow"; ValueType: dword; ValueName: "ElevatedInput"; ValueData: "1"; Check: ElevatedChosen; Flags: uninsdeletekey
Root: HKLM64; Subkey: "SOFTWARE\Rinflow"; ValueType: dword; ValueName: "ElevatedInput"; ValueData: "0"; Check: not ElevatedChosen; Flags: uninsdeletekey

[Run]
; Starting at sign-in is per user: the agent writes it (the value its
; 开机时启动 switch uses) for the user who started setup, not for the
; administrator account setup may run as.
Filename: "{app}\rinflow-agent.exe"; Parameters: "--launch-at-login on"; Tasks: autostart; Flags: runasoriginaluser runhidden waituntilterminated
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Rinflow"""; Flags: runhidden waituntilterminated
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""Rinflow"" dir=in action=allow program=""{app}\rinflow-agent.exe"" enable=yes profile=private,public"; StatusMsg: "{cm:StatusFirewall}"; Flags: runhidden waituntilterminated
; The service only with the feature on (PrepareToInstall removed any old one).
Filename: "{app}\rinflow-service.exe"; Parameters: "--install"; StatusMsg: "{cm:StatusService}"; Check: ElevatedChosen; Flags: runhidden waituntilterminated
Filename: "{app}\Rinflow.exe"; Description: "{cm:RunApp}"; Flags: postinstall nowait skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{app}\rinflow-agent.exe"; Parameters: "--launch-at-login off"; RunOnceId: "RemoveAutostart"; Flags: runhidden waituntilterminated
Filename: "{app}\rinflow-service.exe"; Parameters: "--uninstall"; RunOnceId: "RemoveService"; Flags: runhidden waituntilterminated
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM Rinflow.exe"; RunOnceId: "CloseApp"; Flags: runhidden waituntilterminated
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM rinflow-agent.exe"; RunOnceId: "CloseAgent"; Flags: runhidden waituntilterminated
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Rinflow"""; RunOnceId: "RemoveFirewallRule"; Flags: runhidden waituntilterminated

[UninstallDelete]
; Logs of the service and the input helper. Settings, pairings and this
; PC's identity in each user's %LOCALAPPDATA%\Rinflow stay.
Type: filesandordirs; Name: "{app}\logs"

[Code]
var
  // ElevatedInput as setup found it: -1 when not there (a first install,
  // or after an uninstall), else 0 or 1.
  PreviousElevated: Integer;

function InitializeSetup(): Boolean;
var
  Value: Cardinal;
begin
  if RegQueryDWordValue(HKLM64, 'SOFTWARE\Rinflow', 'ElevatedInput', Value) then
  begin
    if Value <> 0 then
      PreviousElevated := 1
    else
      PreviousElevated := 0;
  end
  else
    PreviousElevated := -1;
  Result := True;
end;

function IsFirstInstall(): Boolean;
begin
  Result := PreviousElevated < 0;
end;

// The task on a first install; on an upgrade, what the settings switch set.
function ElevatedChosen(): Boolean;
begin
  if PreviousElevated < 0 then
    Result := WizardIsTaskSelected('elevatedinput')
  else
    Result := PreviousElevated = 1;
end;

// Before files are replaced (an upgrade): the service and its helper hold
// their executables, and the agent and the app may be running.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  if FileExists(ExpandConstant('{app}\rinflow-service.exe')) then
    Exec(ExpandConstant('{app}\rinflow-service.exe'), '--uninstall', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM Rinflow.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM rinflow-agent.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;
