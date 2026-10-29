#!/bin/bash
#SBATCH --account=research
#SBATCH --partition=compute
#SBATCH --nodes=1
#SBATCH --ntasks=1
#SBATCH --cpus-per-task=1
#SBATCH --mem=256M
#SBATCH --time=00:01:00

set -eu
# Load model-specific modules or activate a runtime here. The worker preserves
# exported setup variables for the registered wrapper.
exec "$1" local-worker --directory "$2"
