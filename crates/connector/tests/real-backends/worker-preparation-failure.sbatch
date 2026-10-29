#!/bin/sh
#SBATCH --partition=debug
#SBATCH --account=research
#SBATCH --time=00:01:00
set -eu
umask 077
printf 'batch preparation starting\n'
# Deliberately prevent workspace creation before the scientific wrapper starts.
printf 'not a directory\n' > "$2/workspace"
exec "$1" local-worker --directory "$2"
