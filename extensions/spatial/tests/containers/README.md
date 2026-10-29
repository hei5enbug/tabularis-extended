# PostGIS container tests

Run from `extensions/spatial` with Docker available:

```sh
corepack pnpm install --frozen-lockfile
corepack pnpm test:containers
```

The tests create and remove their own PostGIS container, use a random password and port, and never read
saved application connections. The image is pinned by digest. Its `linux/amd64` platform requires Docker
emulation on an ARM host.

The tests execute the map's SQL builders against public and non-public PostGIS installations. They cover
quoted identifiers, geometry and geography, coordinate conversion, explicit source SRID, empty results,
and the 1,000-feature limit. UI races and rendering remain covered by `test:ui`.
