#!/bin/bash
# Set your site's account, partition and cluster here before installation.
#SBATCH --nodes=1
#SBATCH --ntasks=1
#SBATCH --cpus-per-task=1
#SBATCH --mem=128M
#SBATCH --time=00:01:00

set -eu
# Load modules, activate Python, or set your model runtime here.
# Exported setup and Slurm allocation variables reach the wrapper.
exec "$1" local-worker --directory "$2"
