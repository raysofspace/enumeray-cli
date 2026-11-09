# REBOUND: an isolated Sun–planet orbit

<a id="run-the-earth-like-orbit-example"></a>

Follow the [REBOUND first-run walkthrough](https://doc.enumeray.com/start/first-run/#rebound-orbit)
to submit `demo-rebound` with `earth-like-orbit` through Enumeray and collect
the orbit plot and trajectory CSV.

REBOUND 5.2.2 follows an Earth-mass planet around a one-solar-mass star. The
planet starts at periapse, its closest point to the star. The model assumes
an isolated Newtonian system and reports positions and velocities relative to
the star in the plane of the orbit.

The default case follows two orbits with a semimajor axis of 1 AU and an
eccentricity of 0.0167.

## Read the trajectory and check the orbit

After collection, open `first-run/bundle.json` to find the plot and CSV paths.
The summary is in `first-run/results/outputs.json`. Save
`first-run/results/provenance.json` with the files you need.

| File | What to look for |
| --- | --- |
| `orbit.png` | The path around the star, with equal scales on both axes. |
| `trajectory.csv` | The 361 samples give time in years, position in AU and velocity in AU per year. |
| `outputs.json` | The orbital period, the closest and farthest distances and the relative changes in energy and angular momentum. |

In the default run, the orbital distances are **0.9833 AU** at periapse and
**1.0167 AU** at apoapse. The fields `periapse_au` and `apoapse_au` are
calculated from the initial orbit parameters. The closest and farthest points
in the CSV depend on the sampled times. For example, a quarter-orbit run
ends before the planet reaches apoapse.

The energy and angular momentum errors in `outputs.json` measure drift
across the solver's samples. Both checks allow drift of at most `1e-9` for this
example.

The samples are evenly spaced in time. In a more elongated orbit, the planet
moves faster near periapse, so its sampled positions are farther apart there.

<a id="change-the-inputs"></a>

## Model inputs

To try different parameters through Enumeray, edit a copy of the
[example inputs](inputs.json) and [start a new analysis with those inputs](https://doc.enumeray.com/users/analyses/).

All three fields take finite JSON numbers.

| Field | Meaning | Allowed range | Default |
| --- | --- | --- | --- |
| `semimajor_axis_au` | Half the long axis of the orbit, in AU | 0.1–5 | 1 |
| `eccentricity` | How far the shape departs from a circle, dimensionless | 0–0.8 | 0.0167 |
| `orbits` | Duration in initial orbital periods | 0.25–10 | 2 |

An `orbits` value less than one ends the calculation before the planet
completes one orbit.

## Numerical setup and checks

REBOUND uses years, AU and solar masses, with a planet mass of `3.003e-6`
solar masses. The IAS15 integrator uses epsilon `1e-9`. It integrates in the
center-of-mass frame with adaptive time steps. The output sampling interval
is fixed separately.

The runner solves Kepler's equation `E-e*sin(E)=M` independently at each saved
time and compares the position with the CSV. The largest position difference,
divided by the semimajor axis, must be at most `1e-8`. The relative drift in
specific orbital energy and angular momentum must each be at most `1e-9`.
These checks apply to the isolated two-body problem. Predicting the position
of the real Earth requires a model that includes other influences.

The wrapper allows one thread, 60 seconds and 8 MiB of combined CSV and PNG
output per run. For a hosted failure, [read the run logs](https://doc.enumeray.com/troubleshooting/execution/).

### Optional reference files

The [REBOUND reference files (ZIP)](sample-run.zip) contain a checked result
for the default inputs. Use the plot, CSV, numerical summary and run record to
compare against a collected result.

## Cite and distribute the example

Upstream REBOUND uses GPL-3.0. Its installed distribution retains the upstream license and redistribution carries its source obligations. The Enumeray example wrapper uses Apache-2.0.

- [REBOUND 5.2.2 source and license](https://github.com/hannorein/rebound/tree/5.2.2)
- [Rein and Liu (2012), REBOUND](https://doi.org/10.1051/0004-6361/201118085)
- [Rein and Spiegel (2015), IAS15](https://doi.org/10.1093/mnras/stu2164)

For questions about the example, contact [Enumeray](mailto:enumeray@raysofspace.com).
