# xmip-core-transport-m-bus

M-Bus transport: EN 13757 wired metering over a serial line — short and long frames, SND_UD and REQ_UD2 answered by RSP_UD, a Stream as variable data records across as many telegrams as it takes; a loopback meter stands in for the line. A technology of [xmip-core-transport](https://github.com/IlleNilsson/xmip-core-transport).

A send target is read by `net::Target` in [xmip-core-library-net](https://github.com/IlleNilsson/xmip-core-library-net), the one reading of a URI every technology calls: scheme, authority, path and decoded query. Until 2026-09-28 it was read through the transport capability's `socket::target`, which split it on its first slash and left the query in the path.

## Acknowledgement

A receive is a read, `REQ_UD2` answered by `RSP_UD`, which consumes nothing
at the meter: it keeps holding the Stream. The verdict therefore has nothing
to tell the meter, whichever it is: a receive cycle that did not complete loses
nothing, and the next read finds the Stream again. The Stream arrives whole.

## Toolchain

`rust-toolchain.toml` pins the toolchain for the whole estate. Do not change it
here.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
