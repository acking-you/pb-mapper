; The Windows installer, pb-mapper-windows-x86_64-setup.exe. fastforge renders
; this file (make_config.yaml names it as script_template) and compiles it with
; Inno Setup 6. It is fastforge's default script, owned here so that what
; ships is reviewed.
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
; The finish page's "Launch" box, as the user who started Setup.
Filename: "{app}\pb_mapper_ui.exe"; Description: "{cm:LaunchProgram,{{DISPLAY_NAME}}}"; Flags: nowait postinstall skipifsilent
