; Inno Setup script for the Windows installer. Build: ISCC.exe /DVersion=0.1.0 packaging\windows\lucarna.iss
; Expects target\release\lucarna.exe; writes target\windows\Lucarna-<version>-windows-x64-setup.exe

#ifndef Version
  #define Version "0.0.0"
#endif

[Setup]
AppId={{5235BE50-3745-4285-BB66-27376C7DFCCE}
AppName=Lucarna
AppVersion={#Version}
AppPublisher=Lucarna
AppPublisherURL=https://github.com/BuilderJoy/lucarna
DefaultDirName={autopf}\Lucarna
DisableProgramGroupPage=yes
; Lets people without admin rights install just for themselves.
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\..\LICENSE
SetupIconFile=..\..\assets\lucarna.ico
UninstallDisplayIcon={app}\lucarna.exe
OutputDir=..\..\target\windows
OutputBaseFilename=Lucarna-{#Version}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\..\target\release\lucarna.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\assets\lucarna.ico"; DestDir: "{app}"
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"

[Icons]
Name: "{autoprograms}\Lucarna"; Filename: "{app}\lucarna.exe"; IconFilename: "{app}\lucarna.ico"
Name: "{autodesktop}\Lucarna"; Filename: "{app}\lucarna.exe"; IconFilename: "{app}\lucarna.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\lucarna.exe"; Description: "{cm:LaunchProgram,Lucarna}"; Flags: nowait postinstall skipifsilent
