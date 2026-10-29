#!/bin/bash
#SBATCH --clusters=enumeray-acceptance
#SBATCH --account=research
#SBATCH --partition=debug
#SBATCH --nodes=1
#SBATCH --ntasks=1
#SBATCH --cpus-per-task=1
#SBATCH --mem=128M
#SBATCH --time=00:00:30

set -eu
export ENUMERAY_SLURM_SETUP=ready
export MODULEPATH=/opt/enumeray/batch-modules
export LD_LIBRARY_PATH=/opt/enumeray/batch-libs
export PYTHONPATH=/opt/enumeray/batch-python
export OMP_NUM_THREADS=3
export PYTHONDONTWRITEBYTECODE=0
printf 'batch setup ready\n'
set +e
"$1" local-worker --directory "$2"
worker_status=$?
printf 'batch worker finished: %s\n' "$worker_status"
exit "$worker_status"
