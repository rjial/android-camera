; Inno Setup script for the Windows installer. Built by build-installer.ps1, which stages the files
; and passes /DAppVersion=<VERSION> /DStageDir=<staging folder> /O<output folder>.
;
; Installs acam.exe and the virtual camera DLL to Program Files (readable by the Frame Server
; services, which load the DLL) and registers the DLL; the OBS plugin goes to
; %ProgramData%\obs-studio\plugins as an optional component. Uninstall undoes all of it.

#ifndef AppVersion
  #error Pass /DAppVersion=<version>
#endif
#ifndef StageDir
  #error Pass /DStageDir=<staging folder>
#endif

[Setup]
; Never change AppId: upgrades and uninstall find the installed copy by it.
AppId={{B9071790-1CA5-4EFA-A50A-0B8B687DEB76}
AppName=ACAM
AppVersion={#AppVersion}
AppVerName=ACAM {#AppVersion}
AppPublisher=ACAM
AppPublisherURL=https://github.com/rjial/android-camera
DefaultDirName={autopf}\ACAM
DefaultGroupName=ACAM
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Admin: the camera DLL is registered machine-wide, which the Frame Server needs.
PrivilegesRequired=admin
MinVersion=10.0
LicenseFile=..\..\LICENSE
OutputBaseFilename=acam-{#AppVersion}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\acam.exe
UninstallDisplayName=ACAM
; Closes a running acam (and OBS, which has the plugin loaded) before replacing files.
CloseApplications=yes

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"

[Components]
Name: "main"; Description: "ACAM app and the ""ACAM Camera"" virtual webcam (Windows 11)"; Types: full compact custom; Flags: fixed
Name: "obs"; Description: "OBS plugin: ""ACAM Camera"" source"; Types: full

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#StageDir}\acam.exe"; DestDir: "{app}"; Components: main; Flags: ignoreversion
; The Frame Server may have the DLL loaded while an app uses the camera; then it is replaced or
; removed after a restart. (Setup refuses to run while such a removal is still pending, so a
; reinstall before that restart can't lose the new DLL.)
Source: "{#StageDir}\vcam_source.dll"; DestDir: "{app}"; Components: main; Flags: ignoreversion regserver restartreplace uninsrestartdelete
Source: "{#StageDir}\README.md"; DestDir: "{app}"; Components: main; Flags: ignoreversion
Source: "{#StageDir}\CHANGELOG.md"; DestDir: "{app}"; Components: main; Flags: ignoreversion
Source: "{#StageDir}\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Components: main; Flags: ignoreversion
Source: "{#StageDir}\obs-plugin\acam-source\*"; DestDir: "{commonappdata}\obs-studio\plugins\acam-source"; Components: obs; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{group}\ACAM"; Filename: "{app}\acam.exe"
Name: "{group}\Uninstall ACAM"; Filename: "{uninstallexe}"
Name: "{autodesktop}\ACAM"; Filename: "{app}\acam.exe"; Tasks: desktopicon

[Run]
; Wi-Fi discovery listens for the phone's UDP broadcast; allow it on private networks only.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""ACAM"""; Flags: runhidden; StatusMsg: "Adding a firewall rule..."
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""ACAM"" dir=in action=allow program=""{app}\acam.exe"" enable=yes profile=private,domain"; Flags: runhidden
Filename: "{app}\acam.exe"; Description: "{cm:LaunchProgram,ACAM}"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""ACAM"""; Flags: runhidden; RunOnceId: "DelFirewallRule"

[UninstallDelete]
; The plugin folders, also when they existed before (an earlier manual install of the plugin).
Type: dirifempty; Name: "{commonappdata}\obs-studio\plugins\acam-source\bin\64bit"
Type: dirifempty; Name: "{commonappdata}\obs-studio\plugins\acam-source\bin"
Type: dirifempty; Name: "{commonappdata}\obs-studio\plugins\acam-source\data\locale"
Type: dirifempty; Name: "{commonappdata}\obs-studio\plugins\acam-source\data"
Type: dirifempty; Name: "{commonappdata}\obs-studio\plugins\acam-source"
