# 2. The mandatory-floor regime is fenced inside the database, not the binary

Once porch enforces that the deterministic floor ran (**ARCH-12**), a `v0.2.x` binary installed
from crates.io would still reconstruct the required producer set from whatever the round contained
and forward branches with no floor at all — and nothing porch ships can make an already-released
binary run new logic. Porch therefore records a minimum-writer protocol in `porch_state_meta` and
enforces it with SQLite triggers over run creation and approval writes, which execute for any
client regardless of its version; a compatible connection registers a `porch_writer_protocol()`
function that the triggers compare against, and a binary lacking it fails those writes closed.
The costs are accepted deliberately: enforcement logic now lives inside operator databases and
outlives the code that wrote it, an incompatible binary gets a blunt SQLite abort rather than
friendly copy (a `Db::open` check supplies the readable message for binaries new enough to run it),
and continued gating on an upgraded state root after downgrading to `v0.2.x` is defined as
unsupported rather than silently permitted. The alternatives were a startup check alone, which
leaves released binaries entirely undefended, and a `NOT NULL` column, which fences run creation
but not approval of a run parked before the upgrade.
