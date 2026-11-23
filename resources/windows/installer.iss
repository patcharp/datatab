; Values are read from the environment so the build workflow does not pass them
; through the Inno-Setup-Action `options` input. Forwarding /D defines whose
; values contain spaces (e.g. "DataTab Nightly") breaks under the action's
; argument splitting, making ISCC mistake the rest for a second script filename.
#ifndef MyAppName
#define MyAppName GetEnv("DATATAB_APP_NAME")
#if MyAppName == ""
#undef MyAppName
#define MyAppName "DataTab"
#endif
#endif
#ifndef MyAppId
#define MyAppId GetEnv("DATATAB_APP_ID")
#if MyAppId == ""
#undef MyAppId
#define MyAppId "{{120589AE-905A-42F5-B3D5-5B740CC474C5}"
#endif
#endif
#ifndef MyAppVersion
#define MyAppVersion GetEnv("DATATAB_APP_VERSION")
#if MyAppVersion == ""
#undef MyAppVersion
#define MyAppVersion "0.1.4"
#endif
#endif
#define MyAppPublisher "PCPLab"
#define MyAppURL "https://datatab.pcplab.me"
#define MyAppExeName "datatab.exe"

[Setup]
AppId={#MyAppId}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DisableProgramGroupPage=yes
LicenseFile=LICENSE-MIT
OutputDir=output
OutputBaseFilename=datatab-windows-amd64-setup
SetupIconFile=datatab.ico
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
MinVersion=10.0
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "datatab.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "datatab.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\datatab.ico"
Name: "{group}\{cm:ProgramOnTheWeb,{#MyAppName}}"; Filename: "{#MyAppURL}"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\datatab.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[Registry]
Root: HKCU; Subkey: "Software\{#MyAppName}"; ValueType: string; ValueName: "InstallPath"; ValueData: "{app}"
Root: HKCU; Subkey: "Software\{#MyAppName}"; ValueType: string; ValueName: "Version"; ValueData: "{#MyAppVersion}"
