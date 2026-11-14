; The Windows installer, pb-mapper-windows-x86_64-setup.exe. fastforge renders
; this file (make_config.yaml names it as script_template) and compiles it with
; Inno Setup 6. It is fastforge's default script, owned here so that what
; ships is reviewed, plus the contract with the in-app update in the [Code]
; section. The release workflow installs the result on its runner and checks
; that contract (.github/workflows/release-ui.yml, build-windows).
;
; The app is pb_mapper_ui.exe, named here rather than left to fastforge, which
; takes the first .exe in the bundle. Since 0.11.0 that is the desktop helper,
; pb-mapper-desktop-helper.exe ("-" sorts before "_"): 0.11.0's Start menu,
; desktop and startup shortcuts and its "Launch pb-mapper" box started the
; helper, which shows nothing and exits, so pb-mapper never appeared.
;
; AppId, the default folder and PrivilegesRequired decide which earlier
; installation an upgrade finds; change none of them. PrivilegesRequired=none
; is Inno's highestAvailable: an administrator's Setup asks Windows for
; permission and installs for all users in Program Files, anyone else's
; installs for that user alone, and an upgrade finds the installation of its
; mode.

[Setup]
AppId={{APP_ID}}
AppVersion={{APP_VERSION}}
AppName={{DISPLAY_NAME}}
AppPublisher={{PUBLISHER_NAME}}
AppPublisherURL={{PUBLISHER_URL}}
AppSupportURL={{PUBLISHER_URL}}
AppUpdatesURL={{PUBLISHER_URL}}
DefaultDirName={{INSTALL_DIR_NAME}}
DisableProgramGroupPage=yes
OutputDir=.
OutputBaseFilename={{OUTPUT_BASE_FILENAME}}
Compression=lzma
SolidCompression=yes
SetupIconFile={{SETUP_ICON_FILE}}
WizardStyle=modern
PrivilegesRequired=none
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; A log in %TEMP% ("Setup Log <date> #<n>.txt") for every run, so a failed
; upgrade can always be explained; the in-app update names its own with /LOG=.
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: {% if CREATE_DESKTOP_ICON != true %}unchecked{% else %}checkedonce{% endif %}
Name: "launchAtStartup"; Description: "{cm:AutoStartProgram,{{DISPLAY_NAME}}}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: {% if LAUNCH_AT_STARTUP != true %}unchecked{% else %}checkedonce{% endif %}

[Files]
Source: "{{SOURCE_DIR}}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{{DISPLAY_NAME}}"; Filename: "{app}\pb_mapper_ui.exe"
Name: "{autodesktop}\{{DISPLAY_NAME}}"; Filename: "{app}\pb_mapper_ui.exe"; Tasks: desktopicon
Name: "{userstartup}\{{DISPLAY_NAME}}"; Filename: "{app}\pb_mapper_ui.exe"; WorkingDir: "{app}"; Tasks: launchAtStartup

[Run]
; The finish page's "Launch" box, as the user who started Setup. A silent
; in-app update opens the app from DeinitializeSetup instead.
Filename: "{app}\pb_mapper_ui.exe"; Description: "{cm:LaunchProgram,{{DISPLAY_NAME}}}"; Flags: nowait postinstall skipifsilent

[Code]
// The in-app update (ui/lib/src/updates/update_installers.dart) starts Setup
// with /UPDATEPID=<its process id> while the app is still open and in front,
// so that the permission Windows asks for (an administrator's Setup starts
// itself again elevated) is asked in front of it, not from behind a closed
// app. The app quits once Setup's log exists, which Setup starts only in the
// process that installs, after that permission. Setup then waits for the app
// to be gone before it replaces a file, and when it ends, installed or not,
// opens the installed app again as the user who started it: the copy in
// {app}, never the copy the app ran from, which since 0.11 can be the
// desktop helper's protected copy of an older release.

const
  SYNCHRONIZE = $00100000;
  WAIT_OBJECT_0 = 0;
  // How long Setup waits for the app to quit before it gives up, having
  // replaced nothing.
  APP_EXIT_TIMEOUT_MS = 60000;

function OpenProcess(DesiredAccess: DWORD; InheritHandle: BOOL; ProcessId: DWORD): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function WaitForSingleObject(Handle: THandle; Milliseconds: DWORD): DWORD;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): BOOL;
  external 'CloseHandle@kernel32.dll stdcall';

// The process of the app this Setup updates; 0 when a person started Setup.
function UpdatedProcess: Integer;
begin
  Result := StrToIntDef(ExpandConstant('{param:UPDATEPID|0}'), 0);
end;

// Called before Setup checks for files in use, so the app is gone by then.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  App: THandle;
begin
  Result := '';
  if UpdatedProcess <= 0 then
    Exit;
  App := OpenProcess(SYNCHRONIZE, False, UpdatedProcess);
  if App = 0 then
  begin
    Log('pb-mapper has already closed');
    Exit;
  end;
  Log('Waiting for pb-mapper to close');
  if WaitForSingleObject(App, APP_EXIT_TIMEOUT_MS) = WAIT_OBJECT_0 then
    Log('pb-mapper has closed')
  else
    Result := 'pb-mapper did not close within a minute, so nothing was installed.';
  CloseHandle(App);
end;

procedure DeinitializeSetup();
var
  App: String;
  ResultCode: Integer;
begin
  if UpdatedProcess <= 0 then
    Exit;
  try
    App := ExpandConstant('{app}\pb_mapper_ui.exe');
    if ExecAsOriginalUser(App, '', ExpandConstant('{app}'), SW_SHOWNORMAL, ewNoWait, ResultCode) then
      Log('Opened ' + App + ' again')
    else
      Log('Could not open ' + App + ' again: ' + SysErrorMessage(ResultCode));
  except
    Log('Could not open pb-mapper again: ' + GetExceptionMessage);
  end;
end;
