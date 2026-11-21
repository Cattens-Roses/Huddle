#define AppName "Huddle"
#define AppPublisher "Huddle"
#define AppExeName "Huddle.exe"
#define AppVersion GetEnv("HUDDLE_VERSION")
#define SourceExe GetEnv("HUDDLE_EXE")
#define OutputDir GetEnv("HUDDLE_DIST")
#define SetupName "Huddle-Setup"

[Setup]
AppId={{A7C3E1B2-4F58-4D1A-9C6E-2B8F0A1D7E44}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
; Per-user install so the folder stays writable for in-app differential updates.
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\{#AppExeName},0
SetupIconFile=..\..\assets\windows\huddle.ico
OutputDir={#OutputDir}
OutputBaseFilename={#SetupName}
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=commandline dialog
WizardStyle=modern
WizardImageFile=..\..\assets\windows\wizard.bmp
WizardSmallImageFile=..\..\assets\windows\wizard-small.bmp
WizardImageStretch=yes

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "{#AppExeName}"; Flags: ignoreversion
Source: "..\..\COPYING"; DestDir: "{app}"; DestName: "LICENSE"; Flags: ignoreversion
Source: "..\..\THIRD-PARTY.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"; IconFilename: "{app}\{#AppExeName}"; IconIndex: 0; AppUserModelID: "app.huddle.desktop"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; IconFilename: "{app}\{#AppExeName}"; IconIndex: 0; AppUserModelID: "app.huddle.desktop"; Tasks: desktopicon
