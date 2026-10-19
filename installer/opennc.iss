; OpenNC Windows installer (Inno Setup 6). Per-user install, no admin prompt.
; Build: ISCC.exe /DAppVersion=0.1.0 installer\opennc.iss   (from the repository root, after cargo build --release
; and with the model unpacked in models\opennc-v1)
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef BuildDir
  #define BuildDir "..\target\release"
#endif

[Setup]
AppId={{7D9E2C1A-5B3F-4E8A-9C6D-0F1E2A3B4C5D}
AppName=OpenNC
AppVersion={#AppVersion}
AppVerName=OpenNC {#AppVersion}
AppPublisher=Mustafa Akcanca
AppPublisherURL=https://github.com/MAkcanca/opennc-app
AppSupportURL=https://github.com/MAkcanca/opennc-app/issues
AppUpdatesURL=https://github.com/MAkcanca/opennc-app/releases
DefaultDirName={localappdata}\Programs\OpenNC
PrivilegesRequired=lowest
DisableProgramGroupPage=yes
DisableDirPage=auto
OutputDir=..\dist
OutputBaseFilename=OpenNC-{#AppVersion}-Setup
SetupIconFile=..\ui\icon.ico
UninstallDisplayIcon={app}\opennc.exe
UninstallDisplayName=OpenNC
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\LICENSE

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "startup"; Description: "Start OpenNC when I sign in to Windows"
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#BuildDir}\opennc.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\opennc-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\models\opennc-v1\*"; DestDir: "{app}\models\opennc-v1"; Flags: ignoreversion recursesubdirs
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\OpenNC"; Filename: "{app}\opennc.exe"
Name: "{autodesktop}\OpenNC"; Filename: "{app}\opennc.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "OpenNC"; ValueData: """{app}\opennc.exe"" --minimized"; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{app}\opennc.exe"; Description: "Launch OpenNC"; Flags: nowait postinstall skipifsilent
; silent (automatic) updates relaunch the app themselves
Filename: "{app}\opennc.exe"; Flags: nowait; Check: WizardSilent

[UninstallDelete]
Type: filesandordirs; Name: "{app}\models"
