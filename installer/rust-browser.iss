; Inno Setup script for Rust Browser (https://jrsoftware.org/isinfo.php).
;
; Build after `cargo build --release`:
;   iscc installer\rust-browser.iss
; The version is read from the .exe; pass /DAppVersion=1.2.3 to override.
;
; What it does:
; - Installs for the current user only (no administrator rights needed), into
;   %LOCALAPPDATA%\Programs\Rust Browser, with Start menu (and optional desktop) shortcuts.
; - Registers Rust Browser with Windows as a web browser, so it can be chosen
;   in Settings > Apps > Default apps (Windows doesn't let apps set this themselves).
; - Installs the Microsoft Edge WebView2 Runtime if the PC doesn't have it.
; - Closes a running Rust Browser before updating it.
; - On uninstall, asks whether to delete browsing data too (kept by default).

#define AppName "Rust Browser"
#define AppExeName "rust-browser.exe"
#define SourceExe "..\target\release\rust-browser.exe"
#ifndef AppVersion
  #define AppVersion GetStringFileInfo(SourceExe, "ProductVersion")
#endif
; Identifiers Windows uses for the browser registration. Don't change them
; between versions, or upgrades leave the old registration behind.
#define ClientKey "RustBrowser"
#define UrlProgId "RustBrowserURL"
#define HtmlProgId "RustBrowserHTML"

[Setup]
; A fixed id: it is how Windows recognises an update of the same app.
AppId={{6F0D8B1E-6C2B-4C1C-9B7A-2E5D4A9C8F31}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Rust Browser contributors
AppPublisherURL=https://github.com/she1bys/rust-browser
AppSupportURL=https://github.com/she1bys/rust-browser/issues
AppUpdatesURL=https://github.com/she1bys/rust-browser/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Per-user install by default; the user may choose "all users" (needs admin).
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=..\LICENSE
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExeName}
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
OutputDir=..\target\installer
OutputBaseFilename=RustBrowser-Setup-{#AppVersion}
; Close a running Rust Browser (via Windows Restart Manager) before replacing it.
CloseApplications=yes
CloseApplicationsFilter={#AppExeName}
RestartApplications=no
ChangesAssociations=yes
VersionInfoVersion={#AppVersion}
VersionInfoProductName={#AppName}
VersionInfoDescription={#AppName} Setup

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Registry]
; ---- Register as a web browser (shows up in Settings > Default apps) ----
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\{#ClientKey}"; ValueType: string; ValueName: ""; ValueData: "{#AppName}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\{#ClientKey}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"",0"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\{#ClientKey}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"""
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\{#ClientKey}\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#AppName}"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\{#ClientKey}\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "A fast, lightweight web browser."
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\{#ClientKey}\Capabilities"; ValueType: string; ValueName: "ApplicationIcon"; ValueData: """{app}\{#AppExeName}"",0"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\{#ClientKey}\Capabilities\StartMenu"; ValueType: string; ValueName: "StartMenuInternet"; ValueData: "{#ClientKey}"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\{#ClientKey}\Capabilities\URLAssociations"; ValueType: string; ValueName: "http"; ValueData: "{#UrlProgId}"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\{#ClientKey}\Capabilities\URLAssociations"; ValueType: string; ValueName: "https"; ValueData: "{#UrlProgId}"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\{#ClientKey}\Capabilities\FileAssociations"; ValueType: string; ValueName: ".htm"; ValueData: "{#HtmlProgId}"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\{#ClientKey}\Capabilities\FileAssociations"; ValueType: string; ValueName: ".html"; ValueData: "{#HtmlProgId}"
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "{#AppName}"; ValueData: "Software\Clients\StartMenuInternet\{#ClientKey}\Capabilities"; Flags: uninsdeletevalue

; ---- How Windows opens web links and .html files with Rust Browser ----
Root: HKA; Subkey: "Software\Classes\{#UrlProgId}"; ValueType: string; ValueName: ""; ValueData: "{#AppName} URL"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\{#UrlProgId}"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\{#UrlProgId}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"",0"
Root: HKA; Subkey: "Software\Classes\{#UrlProgId}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"" ""%1"""
Root: HKA; Subkey: "Software\Classes\{#HtmlProgId}"; ValueType: string; ValueName: ""; ValueData: "{#AppName} HTML Document"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\{#HtmlProgId}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"",0"
Root: HKA; Subkey: "Software\Classes\{#HtmlProgId}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"" ""%1"""
; Offer Rust Browser in "Open with" for web pages, without taking them over.
Root: HKA; Subkey: "Software\Classes\.htm\OpenWithProgids"; ValueType: string; ValueName: "{#HtmlProgId}"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.html\OpenWithProgids"; ValueType: string; ValueName: "{#HtmlProgId}"; ValueData: ""; Flags: uninsdeletevalue

[Run]
; After an update from inside the browser (`/RELAUNCH`, see src/updater.rs):
; start it again with the tabs it had open, as the signed-in user (not as admin).
Filename: "{app}\{#AppExeName}"; Parameters: "--restore-session"; Flags: nowait runasoriginaluser; Check: RelaunchRequested
Filename: "{app}\{#AppExeName}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; Windows only lets you, not an app, change the default browser: this opens
; the right page of Settings.
Filename: "ms-settings:defaultapps?registeredAppUser={#AppName}"; Description: "Make {#AppName} my default browser (opens Windows Settings)"; Flags: shellexec postinstall skipifsilent unchecked nowait

[Code]
const
  WebView2ClientKey = 'Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';
  WebView2BootstrapperUrl = 'https://go.microsoft.com/fwlink/p/?LinkId=2124703';

var
  DownloadPage: TDownloadWizardPage;

{ True when the browser itself started this update and wants to be reopened. }
function RelaunchRequested: Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), '/RELAUNCH') = 0 then
      Result := True;
end;

{ True if the WebView2 Runtime is installed, for this user or for all users. }
function WebView2Installed: Boolean;
var
  Version: String;
begin
  Result :=
    (RegQueryStringValue(HKLM, 'SOFTWARE\WOW6432Node\' + WebView2ClientKey, 'pv', Version) and (Version <> '') and (Version <> '0.0.0.0')) or
    (RegQueryStringValue(HKLM, 'SOFTWARE\' + WebView2ClientKey, 'pv', Version) and (Version <> '') and (Version <> '0.0.0.0')) or
    (RegQueryStringValue(HKCU, 'Software\' + WebView2ClientKey, 'pv', Version) and (Version <> '') and (Version <> '0.0.0.0'));
end;

procedure InitializeWizard;
begin
  DownloadPage := CreateDownloadPage(SetupMessage(msgWizardPreparing), 'Downloading the Microsoft Edge WebView2 Runtime, which Rust Browser uses to show web pages...', nil);
end;

{ Before installing: fetch and run Microsoft's WebView2 installer if needed. }
function NextButtonClick(CurPageID: Integer): Boolean;
var
  ResultCode: Integer;
begin
  Result := True;
  if (CurPageID = wpReady) and not WebView2Installed then
  begin
    DownloadPage.Clear;
    DownloadPage.Add(WebView2BootstrapperUrl, 'MicrosoftEdgeWebview2Setup.exe', '');
    DownloadPage.Show;
    try
      try
        DownloadPage.Download;
        if not Exec(ExpandConstant('{tmp}\MicrosoftEdgeWebview2Setup.exe'), '/silent /install', '', SW_HIDE, ewWaitUntilTerminated, ResultCode) or not WebView2Installed then
          MsgBox('The Microsoft Edge WebView2 Runtime could not be installed (code ' + IntToStr(ResultCode) + ').' + #13#10 +
                 'Rust Browser needs it to show web pages. You can install it from https://developer.microsoft.com/microsoft-edge/webview2/', mbError, MB_OK);
      except
        MsgBox('Downloading the Microsoft Edge WebView2 Runtime failed: ' + GetExceptionMessage + #13#10 +
               'Rust Browser needs it to show web pages. You can install it from https://developer.microsoft.com/microsoft-edge/webview2/', mbError, MB_OK);
      end;
    finally
      DownloadPage.Hide;
    end;
  end;
end;

{ After uninstalling: offer to delete browsing data (kept unless the user says yes). }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    DataDir := ExpandConstant('{localappdata}\RustBrowser');
    if DirExists(DataDir) and not UninstallSilent then
      if MsgBox('Also delete your Rust Browser data (bookmarks, history, settings, saved passwords, cookies and cache)?' + #13#10#13#10 +
                'Choose No to keep it for a future install.', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(DataDir, True, True, True);
  end;
end;
