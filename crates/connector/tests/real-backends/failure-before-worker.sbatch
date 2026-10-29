#!/bin/bash
#SBATCH --clusters=enumeray-acceptance
#SBATCH --account=research
#SBATCH --partition=debug
#SBATCH --nodes=1
#SBATCH --ntasks=1
#SBATCH --cpus-per-task=1
#SBATCH --mem=128M
#SBATCH --time=00:00:30

# Accepted allocation failure before the Enumeray worker or wrapper starts.
exit 23
