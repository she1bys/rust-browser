<#
.SYNOPSIS
  Clicks through Rust Browser's main features the way a person would and
  reports PASS or FAIL for each.

.DESCRIPTION
  Uses Windows UI Automation (the accessibility interface screen readers use)
  to press the browser's real buttons, so it needs no mouse or keyboard and
  works with the window in the background.

  It never touches your own browser data or the internet: the browser runs
  with its own temporary profile (RUST_BROWSER_PROFILE) as a separate
  instance, and test pages come from a tiny local web server (tests\site).

  Build first, then run:
    cargo build --release
    powershell -ExecutionPolicy Bypass -File tests\ui-smoke.ps1

  -Long also waits for the popup's web view to be released after two idle
  minutes (adds about 2.5 minutes).
#>
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\target\release\rust-browser.exe'),
  [int]$Port = 8767,
  [switch]$Long,
  [switch]$KeepOpen
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$AE = [System.Windows.Automation.AutomationElement]
$Scope = [System.Windows.Automation.TreeScope]
$Exe = (Resolve-Path $Exe).Path

$profileDir = Join-Path $env:TEMP ("rust-browser-ui-test-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory $profileDir | Out-Null
$env:RUST_BROWSER_PROFILE = $profileDir
$site = "http://127.0.0.1:$Port"

$results = New-Object System.Collections.Generic.List[object]
function Check([string]$name, [scriptblock]$test) {
  $sw = [Diagnostics.Stopwatch]::StartNew()
  try {
    $detail = & $test
    $results.Add([pscustomobject]@{ Result = 'PASS'; Test = $name; Detail = "$detail" })
    Write-Host ("PASS  {0,-46} {1}" -f $name, $detail) -ForegroundColor Green
  } catch {
    $results.Add([pscustomobject]@{ Result = 'FAIL'; Test = $name; Detail = $_.Exception.Message })
    Write-Host ("FAIL  {0,-46} {1}" -f $name, $_.Exception.Message) -ForegroundColor Red
  }
}

function Wait-Until([scriptblock]$condition, [int]$timeoutMs = 8000, [string]$what = 'condition') {
  $sw = [Diagnostics.Stopwatch]::StartNew()
  while ($sw.ElapsedMilliseconds -lt $timeoutMs) {
    $value = & $condition
    if ($value) { return $value }
    Start-Sleep -Milliseconds 150
  }
  throw "timed out waiting for $what"
}

# All top-level windows of the browser (main window and its popup).
function Get-BrowserWindows {
  $cond = New-Object System.Windows.Automation.PropertyCondition ($AE::ProcessIdProperty), $script:browser.Id
  $AE::RootElement.FindAll($Scope::Children, $cond)
}

# The first element whose name matches -like $pattern (and optional control type).
function Find-Element([string]$pattern, [string]$type = '', [int]$timeoutMs = 8000) {
  Wait-Until -timeoutMs $timeoutMs -what "'$pattern'" {
    foreach ($window in Get-BrowserWindows) {
      $cond = if ($type) {
        New-Object System.Windows.Automation.PropertyCondition ($AE::ControlTypeProperty), ([System.Windows.Automation.ControlType]::$type)
      } else { [System.Windows.Automation.Condition]::TrueCondition }
      foreach ($el in $window.FindAll($Scope::Descendants, $cond)) {
        if ($el.Current.Name -like $pattern) { return $el }
      }
    }
    $null
  }
}

function Test-Element([string]$pattern, [string]$type = '') {
  try { [bool](Find-Element $pattern $type 600) } catch { $false }
}

# Buttons are "invoked", switches "toggled", and buttons that open a popup
# (like the menu button) "expanded".
function Press($element) {
  $patterns = @($element.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName })
  if ($patterns -contains 'InvokePatternIdentifiers.Pattern') {
    $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
  } elseif ($patterns -contains 'TogglePatternIdentifiers.Pattern') {
    $element.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
  } elseif ($patterns -contains 'ExpandCollapsePatternIdentifiers.Pattern') {
    $element.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
  } else {
    throw "'$($element.Current.Name)' can't be pressed (supports: $($patterns -join ', '))"
  }
}

function Count-Elements([string]$name) {
  $cond = New-Object System.Windows.Automation.PropertyCondition ($AE::NameProperty), $name
  $n = 0
  foreach ($window in Get-BrowserWindows) { $n += $window.FindAll($Scope::Descendants, $cond).Count }
  $n
}

function Click([string]$pattern, [string]$type = 'Button') { Press (Find-Element $pattern $type) }

# The saved settings, or nothing while the browser hasn't written them yet.
function Get-Settings {
  $file = Join-Path $profileDir 'settings.json'
  if (-not (Test-Path $file)) { return $null }
  try { Get-Content $file -Raw | ConvertFrom-Json } catch { $null }
}

# Engine processes started for this browser (WebView2 runs each page group,
# the GPU and so on in separate msedgewebview2.exe processes).
function Get-EngineProcesses {
  $all = Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'"
  $ours = @{}; $ours[[int]$script:browser.Id] = $true
  do {
    $added = $false
    foreach ($p in $all) {
      if (-not $ours.ContainsKey([int]$p.ProcessId) -and $ours.ContainsKey([int]$p.ParentProcessId)) {
        $ours[[int]$p.ProcessId] = $true; $added = $true
      }
    }
  } while ($added)
  @($all | Where-Object { $ours.ContainsKey([int]$_.ProcessId) })
}
function Get-Renderers { @(Get-EngineProcesses | Where-Object { $_.CommandLine -like '*--type=renderer*' }) }

function Open-Url([string]$url) {
  # A second launch hands the address to the running browser, which opens it in a new tab.
  Start-Process $Exe -ArgumentList $url | Out-Null
}

# Paths are quoted: Start-Process joins the arguments with spaces.
$server = Start-Process powershell -WindowStyle Hidden -PassThru -ArgumentList @(
  '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', ('"' + (Join-Path $PSScriptRoot 'test-server.ps1') + '"'),
  '-Root', ('"' + (Join-Path $PSScriptRoot 'site') + '"'), '-Port', $Port)
$browser = $null

try {
  Write-Host "Rust Browser UI test - $Exe" -ForegroundColor Cyan
  Write-Host "Test profile: $profileDir`n"
  Wait-Until -what 'test web server' { try { (Invoke-WebRequest "$site/article.html" -UseBasicParsing -TimeoutSec 2).StatusCode -eq 200 } catch { $false } } | Out-Null

  Check 'Starts and shows the New Tab page' {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $script:browser = Start-Process $Exe -ArgumentList 'about:blank' -PassThru
    Find-Element 'Search or enter web address' | Out-Null
    "ready in $($sw.ElapsedMilliseconds) ms"
  }

  $renderersAtStart = 0
  Check 'Popup web view is not created at start-up' {
    Start-Sleep -Seconds 2
    $script:renderersAtStart = (Get-Renderers).Count
    if (Test-Element 'Settings Ctrl*' 'MenuItem') { throw 'menu items exist before the menu was opened' }
    "$($script:renderersAtStart) page processes running"
  }

  Check 'Menu opens (first use creates its web view)' {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    Click 'Menu'
    Find-Element 'Settings Ctrl*' 'MenuItem' | Out-Null
    "$($sw.ElapsedMilliseconds) ms; page processes: $((Get-Renderers).Count)"
  }

  Check 'Menu > Settings opens Settings' {
    Click 'Settings Ctrl*' 'MenuItem'
    Find-Element 'Block ads and trackers' | Out-Null
    'Settings panel shown'
  }

  Check 'Ad blocker switch saves the setting' {
    # The switch (a button with a toggle), not the row's text label of the same name.
    Click 'Block ads and trackers' 'Button'
    Wait-Until -what 'blockAds = false' { $s = Get-Settings; $s -and $s.blockAds -eq $false } | Out-Null
    Click 'Block ads and trackers' 'Button'
    Wait-Until -what 'blockAds = true' { $s = Get-Settings; $s -and $s.blockAds -eq $true } | Out-Null
    'off, then on again'
  }

  Check 'Settings shows Efficiency mode and Updates' {
    Find-Element 'Efficiency mode' | Out-Null
    Find-Element 'Check for updates' 'Button' | Out-Null
    'both present'
  }

  Check 'Settings > Done closes Settings' {
    Click 'Done'
    Wait-Until -what 'Settings to close' { -not (Test-Element 'Block ads and trackers') } | Out-Null
    'closed'
  }

  Check 'Opens a page passed on the command line' {
    Open-Url "$site/article.html"
    Wait-Until -what 'article title' { (Get-Process -Id $browser.Id).MainWindowTitle -like 'Test article*' } | Out-Null
    (Get-Process -Id $browser.Id).MainWindowTitle
  }

  Check 'Reader mode opens and closes' {
    # The page stays underneath the reader, so compare counts: the article
    # heading appears twice (page + reader), the sidebar link only once.
    $headingBefore = Count-Elements 'How a small browser keeps its memory low'
    $sidebarBefore = Count-Elements 'Another story'
    Click 'Reader mode'
    Find-Element 'Close reader mode (Esc)' | Out-Null
    Wait-Until -what 'the article in the reader' { (Count-Elements 'How a small browser keeps its memory low') -gt $headingBefore } | Out-Null
    if ((Count-Elements 'Another story') -gt $sidebarBefore) { throw 'the sidebar was copied into the reader' }
    Click 'Close reader mode (Esc)'
    Wait-Until -what 'reader to close' { -not (Test-Element 'Close reader mode (Esc)') } | Out-Null
    'article shown without the sidebar'
  }

  Check 'Ad blocker blocks ads and shows the count' {
    Open-Url "$site/ads.html"
    $badge = Find-Element '*blocked on this page' '' 12000
    $badge.Current.Name
  }

  Check 'Bookmark star saves a bookmark' {
    Click 'Bookmark this page'
    Wait-Until -what 'bookmark saved' {
      $file = Join-Path $profileDir 'bookmarks.json'
      (Test-Path $file) -and ((Get-Content $file -Raw) -like '*ads.html*')
    } | Out-Null
    'saved to bookmarks.json'
  }

  Check 'Split view opens' {
    Click 'Split view'
    Find-Element 'Swap split panes' | Out-Null
    'two panes'
  }

  Check 'Memory manager opens' {
    Click 'Memory*'
    Find-Element 'Free up memory*' 'Button' | Out-Null
    Click 'Memory*'
    'opened and closed'
  }

  Check 'New tab button adds a tab; Close tab removes it' {
    $before = @((Get-BrowserWindows)[0].FindAll($Scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition ($AE::NameProperty), 'Close tab'))).Count
    Click 'New tab'
    Wait-Until -what 'a new tab' {
      @((Get-BrowserWindows)[0].FindAll($Scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition ($AE::NameProperty), 'Close tab'))).Count -eq $before + 1
    } | Out-Null
    $closes = (Get-BrowserWindows)[0].FindAll($Scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition ($AE::NameProperty), 'Close tab'))
    Press $closes[$closes.Count - 1]
    Wait-Until -what 'the tab to close' {
      @((Get-BrowserWindows)[0].FindAll($Scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition ($AE::NameProperty), 'Close tab'))).Count -eq $before
    } | Out-Null
    "$before tabs -> $($before + 1) -> $before"
  }

  if ($Long) {
    Check 'Idle popup web view is released after 2 minutes' {
      $open = (Get-Renderers).Count
      Start-Sleep -Seconds 135
      $later = (Get-Renderers).Count
      if ($later -ge $open) { throw "page processes: $open before, $later after waiting" }
      "page processes: $open -> $later"
    }
  }

  Check 'Engine memory (all processes, private working set)' {
    $ids = @((Get-EngineProcesses).ProcessId) + $browser.Id
    $mb = 0
    foreach ($id in $ids) { try { $mb += (Get-Process -Id $id).PrivateMemorySize64 } catch {} }
    "{0:N0} MB in {1} processes" -f ($mb / 1MB), $ids.Count
  }

  if (-not $KeepOpen) {
    Check 'Closing the window exits the browser' {
      Click 'Close'
      Wait-Until -what 'the browser to exit' { $browser.HasExited } -timeoutMs 10000 | Out-Null
      'exited'
    }
  }
} finally {
  if (-not $KeepOpen -and $browser -and -not $browser.HasExited) { Stop-Process -Id $browser.Id -Force -ErrorAction SilentlyContinue }
  Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
  if (-not $KeepOpen) {
    Start-Sleep -Seconds 2
    Remove-Item $profileDir -Recurse -Force -ErrorAction SilentlyContinue
  }
}

$failed = @($results | Where-Object Result -eq 'FAIL').Count
Write-Host ("`n{0} passed, {1} failed" -f ($results.Count - $failed), $failed) -ForegroundColor $(if ($failed) { 'Red' } else { 'Green' })
exit $failed
