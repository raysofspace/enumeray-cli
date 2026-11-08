# REBOUND: an isolated Sun–planet orbit

How do the size and shape of an orbit change a planet's trajectory? This REBOUND 5.2.2 example lets you vary an orbit around a one-solar-mass star. The planet has a mass of `3.003e-6` solar masses and begins at its closest point to the star, called periapse.

The IAS15 integrator solves an isolated Newtonian two-body problem. The orbit is in the xy plane. The calculation uses the center-of-mass frame and reports positions and velocities relative to the star. This example omits other planets, relativistic effects, tides and collisions. Use it to study the two-body orbit. Predicting the position of the real Earth requires a different model.

## Run the Earth-like orbit example

Check [hosted trial availability](https://doc.enumeray.com/start/hosted-release/).
Once you have access and an approved CLI for the trial's API address, follow
[your first run](https://doc.enumeray.com/start/first-run/). Choose the
[REBOUND orbit example](https://doc.enumeray.com/start/first-run/#rebound-orbit).
The hosted model ID is `demo-rebound` and the example is `earth-like-orbit`. It uses a semimajor axis of 1 AU, an eccentricity of 0.0167 and follows two orbits.

When the walkthrough finishes, read the files in its `first-run` folder.

## Read the trajectory and check the orbit

Open `first-run/bundle.json` for the local paths to these outputs.

| Output name | What to open |
| --- | --- |
| `trajectory_csv` | `trajectory.csv` contains 361 rows of `time_years,x_au,y_au,vx_au_per_year,vy_au_per_year`. |
| `orbit_plot_png` | `orbit.png` shows the path around the star, with equal scales on both axes. |

The CLI saves each file under its own `results/files/<artifact-uuid>/` directory. The summary at `first-run/results/outputs.json` includes the initial analytic period and closest and farthest distances from the star. For the default inputs, the distance ranges from **0.9833 AU** at periapse to **1.0167 AU** at apoapse. The fields `max_relative_energy_error` and `max_relative_angular_momentum_error` show the largest sampled fractional changes in specific orbital energy and angular momentum.

A partial orbit can end before the planet returns to its starting point. At high eccentricity, samples are farther apart near periapse because the planet moves faster there. The samples are evenly spaced in time, independently of the integrator's adaptive timestep.

Keep `first-run/results/outputs.json` and `first-run/results/provenance.json` with the CSV and plot. Demo data is temporary, so collect the files you need.

## Change the inputs

Edit a copy of this project's `inputs.json` and initialize another analysis directory with `--inputs inputs.json`. Keep all three fields and enter finite JSON numbers.

| Field | Meaning | Allowed range | Default |
| --- | --- | --- | --- |
| `semimajor_axis_au` | Half the long axis of the orbit, in AU | 0.1–5 | 1 |
| `eccentricity` | How far the shape departs from a circle, dimensionless | 0–0.8 | 0.0167 |
| `orbits` | Duration in initial orbital periods | 0.25–10 | 2 |

The schema rejects missing or extra fields, strings, booleans and out-of-range values. The masses, orientation, integrator and number of output samples stay fixed.

## Numerical setup and checks

REBOUND sets the units to years, AU and solar masses before adding the particles and supplies the corresponding gravitational constant. IAS15 uses epsilon `1e-9`. The calculation requires no random seed, external ephemeris or runtime download.

The tests solve Kepler's equation `E-e*sin(E)=M` independently at each saved time. The maximum position error relative to semimajor axis must be at most `1e-8`. Relative drift in specific energy and angular momentum must each be at most `1e-9`. Tests cover circular and maximum-eccentricity cases. The example qualification also checks all parameter corners. These comparisons check the isolated problem and its wrapper. Plot bytes may vary with plotting libraries and fonts.

The wrapper permits one thread, 60 seconds and 8 MiB combined CSV and PNG output per run. Solver errors, nonfinite values, failed conservation checks and exceeded limits produce a failed result. Read the run's failure and logs if that happens. Demo capacity is shared and queue limits apply. [Troubleshoot a run](https://doc.enumeray.com/troubleshooting/execution/)

## Cite and distribute the example

Upstream REBOUND uses GPL-3.0. Its installed distribution retains the upstream license and redistribution carries its source obligations. The Enumeray example wrapper uses Apache-2.0.

- [REBOUND 5.2.2 source and license](https://github.com/hannorein/rebound/tree/5.2.2)
- [Rein and Liu (2012), REBOUND](https://doi.org/10.1051/0004-6361/201118085)
- [Rein and Spiegel (2015), IAS15](https://doi.org/10.1093/mnras/stu2164)

For questions about the example, contact [Enumeray](mailto:enumeray@raysofspace.com).
