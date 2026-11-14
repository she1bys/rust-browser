# A tiny web server for the UI tests: serves the files in -Root on
# http://127.0.0.1:<Port>/ until the process is stopped. Only this PC can reach it.
param([Parameter(Mandatory)][string]$Root, [int]$Port = 8767)

$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
$listener.Start()
while ($true) {
  $client = $listener.AcceptTcpClient()
  try {
    $stream = $client.GetStream(); $stream.ReadTimeout = 5000
    $reader = New-Object IO.StreamReader($stream)
    $line = $reader.ReadLine()
    while (($h = $reader.ReadLine()) -ne $null -and $h -ne '') {}
    $path = if ($line -match '^GET /([^ ?#]*)') { $Matches[1] } else { '' }
    if ($path -eq '') { $path = 'article.html' }
    $file = Join-Path $Root $path
    $ok = ($path -notmatch '\.\.') -and (Test-Path $file -PathType Leaf)
    $body = if ($ok) { [IO.File]::ReadAllBytes($file) } else { [Text.Encoding]::UTF8.GetBytes('Not found') }
    $type = if ($path -like '*.html') { 'text/html; charset=utf-8' } else { 'application/octet-stream' }
    $status = if ($ok) { '200 OK' } else { '404 Not Found' }
    $head = [Text.Encoding]::ASCII.GetBytes("HTTP/1.1 $status`r`nContent-Type: $type`r`nContent-Length: $($body.Length)`r`nCache-Control: no-store`r`nConnection: close`r`n`r`n")
    $stream.Write($head, 0, $head.Length); $stream.Write($body, 0, $body.Length); $stream.Flush()
  } catch {} finally { $client.Close() }
}
