#!/bin/sh
#SBATCH --partition=enumeray-nonexistent
set -eu
exec "$1" local-worker --directory "$2"
