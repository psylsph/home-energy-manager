#!/bin/bash
# Launch GivEnergy-Local.app bypassing Gatekeeper
# (runs the binary directly instead of using `open`)
exec "/Applications/GivEnergy-Local.app/Contents/MacOS/givenergy-local" "$@"
