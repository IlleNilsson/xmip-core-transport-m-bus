# xmip-core-transport-m-bus

M-Bus transport: EN 13757 wired metering over a serial line — short and long frames, SND_UD and REQ_UD2 answered by RSP_UD, a Stream as variable data records across as many telegrams as it takes; a loopback meter stands in for the line. A technology of [xmip-core-transport](https://github.com/IlleNilsson/xmip-core-transport).

A send target is read by `net::Target` in [xmip-core-library-net](https://github.com/IlleNilsson/xmip-core-library-net), the one reading of a URI every technology calls: scheme, authority, path and decoded query. Until 2026-09-28 it was read through the transport capability's `socket::target`, which split it on its first slash and left the query in the path.

## The line

A Location's address is the serial port the meter is on. The master transmits
a telegram and receives the meter's answer through that one port, which
`xmip-core-transport-serial` keeps open between the two, so an answer that
comes before the receive is read; every receive waits the Location's `timeout`,
5 s where it is left out, the serial port's own (`serial::TIMEOUT`), and a meter
that sends nothing in that time did not answer. A write to the port waits the
port's own 5 s.

Every M-Bus Location on a node naming one port reads its meter through that
port's one handle (ADR-0064, amendment 2026-11-15): the master hands what the
node's Locations share on to its line (`Configured::shared_by`,
`Line::shared_on`), so several meters on one multi-drop bus, each its own
Location, are all read, where until 2026-11-15 every Location but the first
was refused the port. Each telegram and its answer are one exchange on the
line (`Line::exchange`), the port held across both, so a meter's answer
reaches only the master that asked; each exchange runs at its own Location's
`baud`.

The frame count bit (EN 13757-2) is the meter's, by its primary address: on a
node it is kept with the port, under its path in what the node's Locations
share, so every Location reaching a meter alternates one bit (the owner,
2026-11-15). It is held across each call, toggled once the meter answered, and
set for the meter's next call by its reset (`SND_NKE`). On a line nothing is
shared on, the bits are the Location's own. Until 2026-11-15 each Location kept
one bit for every meter it called, so two Locations on one meter toggled it
apart and the meter took the second's telegram for a repeat of the first's.

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
