; Inno Setup 6 script for Lumen.
;
; Build the app first, then compile this script:
;   cargo build --release
;   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\lumen.iss
; The installer is written to target\installer\LumenSetup-<version>.exe.
;
; Installs for the current user by default (no admin prompt); the setup
; dialog offers "install for all users" too. Uninstalling removes the
; program only: your library database, thumbnails, logs (%LOCALAPPDATA%\Lumen)
; and settings (%APPDATA%\Lumen) stay unless you delete them.

#define AppName "Lumen"
#define AppVersion "0.1.0"
#define AppExe "lumen.exe"
#define Root ".."

[Setup]
AppId={{6E8B7C1F-3A54-4D2E-9B1A-5C7E2F0D4A93}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Lumen
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#Root}\target\installer
OutputBaseFilename=LumenSetup-{#AppVersion}
SetupIconFile={#Root}\assets\lumen.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Root}\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
