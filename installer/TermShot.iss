#ifndef AppVersion
  #error Pass the package version with /DAppVersion
#endif
#ifndef AppExe
  #define AppExe "..\target\release\TermShot.exe"
#endif

[Setup]
AppId={{3F540F7F-90D1-48A4-AE71-D40805611DBF}
AppName=TermShot
AppVersion={#AppVersion}
AppPublisher=zhuangqi
AppPublisherURL=https://github.com/zhuangqi/TermShot
AppUpdatesURL=https://github.com/zhuangqi/TermShot/releases/latest
DefaultDirName={localappdata}\Programs\TermShot
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
MinVersion=10.0
SetupIconFile=..\assets\TermShot.ico
UninstallDisplayIcon={app}\TermShot.exe
OutputDir=..\target\installer
OutputBaseFilename=TermShot-{#AppVersion}-Windows-x64-Setup
VersionInfoVersion={#AppVersion}
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
RestartApplications=no

[Files]
Source: "{#AppExe}"; DestDir: "{app}"; DestName: "TermShot.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\TermShot"; Filename: "{app}\TermShot.exe"; WorkingDir: "{app}"

[Run]
Filename: "{app}\TermShot.exe"; Description: "{cm:LaunchProgram,TermShot}"; Flags: nowait postinstall skipifsilent unchecked

[Code]
const
  StartupKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  StartupValue = 'CliScreenshot';

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  StartupCommand: String;
  InstalledCommand: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    InstalledCommand := '"' + ExpandConstant('{app}\TermShot.exe') + '" --startup';
    if RegQueryStringValue(HKCU, StartupKey, StartupValue, StartupCommand) and
      (CompareText(StartupCommand, InstalledCommand) = 0) then
      RegDeleteValue(HKCU, StartupKey, StartupValue);
  end;
end;
