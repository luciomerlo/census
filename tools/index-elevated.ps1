param([string]$Volume = "C")
$here = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $here "target\release\censusindex.exe"
$out = Join-Path $here "index.txt"
& $exe index --volume $Volume *> $out
"# exit=$LASTEXITCODE" | Add-Content $out
