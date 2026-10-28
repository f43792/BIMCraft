# Point clouds

On the roadmap under 0.9 Interoperability. A project holds scans and places them, the 3D view, plans and sections draw them,
plans snap to them, and the desktop brings them in (Insert > Point Cloud). IFC leaves them out, as Revit's export does: IFC4 has
no point cloud, and `ifc_leaves_scans_out_as_revit_does` keeps the file what it was without them.

Point clouds (`cloud.rs`) come from public specifications: PTS and XYZ text, PLY (text or either byte order) and ASPRS LAS 1.0
to 1.4 uncompressed, and ASTM E57 (`Format::from_extension` says why LAZ and ReCap's RCP/RCS don't). A file is read a piece at a
time through a `Source` (`cloud/source.rs`: bytes asked for where they are, `FileSource` for one on disk; text goes by in lines
and LAS and PLY in records, a few megabytes at once), so a file of any size is read without being held. `cloud::read_from` parses a file twice,
once to measure it (`Stats`: the count, the bounds, the ranges of intensity and colour) and once to keep it: every `stride`th
point so `Options::cap` is never passed, then one in each cube of `spacing`, the same every time. A `Scan` keeps its points as
`f32` about `origin`, the middle of its bounds, so a survey's coordinates far off keep their millimetres within `MAX_SPAN`;
colour comes in eight bits whether the file has eight, sixteen or nought to one, and intensity alone is grey.

E57 (ASTM E2807, `cloud/e57.rs`) is read from the standard with no library: the 48-byte header, pages of 1024 bytes whose last
four are a CRC-32C (`Paged` reads the 1020 logical bytes of each; the checksums aren't checked, so a page that lies reads as
points that lie, never a crash), and the XML section, read by a small parser of its own (elements, attributes, text, CDATA;
capped in depth and size). Every `data3D` scan comes in, placed by its `pose` (a quaternion and a translation), as one `Scan`
whose `read` is the sum of their `recordCount`s. Each scan's points are a CompressedVector: a section header, then data packets
whose bytestreams, one for each field of the `prototype`, are bit-packed and run on from packet to packet; `Pack` decodes
Float (single or double), Integer and ScaledInteger, and only bit-packing, the one codec E57 writes, is read. Cartesian or
spherical coordinates both come in, a point marked invalid doesn't, and colour is scaled from `colorLimits` (or its field's
range) to eight bits. `images2D` is passed over. A file is held to `Caps` before anything is unpacked: up to 4096 scans, 100 million
points in all of them (2^32: the points stream through, so the cap is on the time a lie costs), 32 fields to a point, no two scans on one section, a section with room for the bits its count asks for (a
field of no bits is legal, so eight points can share a byte), and no stream more than 4 MiB ahead of the others. The tests write E57 files of their own (`e57` in the module's tests), with
values that cross packets and pages; the reference survey's 177 MB file reads its 6.6 million points in under two seconds.

A project holds each scan once, by name, as a `Cloud` in `Model::clouds` (a library entry of kind `Cloud`, so it has an id and a
GUID): its points and colours shared behind an `Arc`, the file it came from, and where its middle was there. A project file holds
it as `CloudFile`, the points as little-endian `f32` and the colours as bytes, each deflated and in base64, packed once and kept
(`Cloud::file`); coming back, every check `Cloud::new` makes is made again, and what unpacks past `MAX_CLOUD_POINTS` stops there.
A project holds up to `MAX_CLOUDS` scans and `MAX_CLOUD_POINTS` points among them (`Model::check`), so the `point_cloud` command
thins a new one to what the others leave. A project file is held to the same while it is read, not after: `Model::clouds` reads
through `cloud::deserialize_scans`, which refuses the entry past the count, and the scan that takes the points past the budget,
before unpacking the next (unpacking is where the memory goes, and the file is read twice, by the lenient walk and the strict read).

A survey's stations come in as one scan, as ReCap gathers a project's scans and Revit links the project: `path` may be a list of
up to `MAX_STATIONS` files. Each is read with an even share of what the project leaves, and `cloud::merge` puts them about the
middle of them all, keeps one point in each cube of `spacing` across them (so where stations overlap they aren't kept twice), and
greys a file with no colours among ones with. The `Cloud` keeps a `Station` for each file, its name and its share of the points
in order, so a view may one day show or hide a station; it is known by the first file's name, and the element's JSON lists the
stations. A list of one is that file alone.

A `PointCloud` element places a scan, as a component places a family: its `cloud` (a library name, a GUID in a file), its `level`,
its middle at `at` and `offset` over the level, turned `angle` degrees about it; `Model::cloud_place` is that transform and
`Model::cloud_box` the box round what it places. It holds no `params`, so phases, design options and paint don't reach it. It moves,
turns and copies as any element; it doesn't mirror, since a scan is what was measured. The `point_cloud` command brings a file in
(`path`) or places a held scan again (`cloud`); by default its middle goes on `at` and its foot on the level (Revit's Auto - Center
to Center), and with `origin: file` where the file's coordinates put it (Origin to Origin) when that is within `MAX_COORD`. A scan
that nothing places any more goes after the command that left it so (`Model::unplaced_clouds`, in `Session::execute`), and undo
brings it back with its element. Insert from File brings the scan with the element. IFC export leaves scans out.

The 3D view (`view3d.rs`) keeps each placed scan as a `Scan`: its points and colours shared with the model's, and where they
go. After the opaque surfaces are rasterised, `Scene::dots` draws each point as a square [`DOT`] pixels wide into the same
depth buffer, so what is nearer hides a scan and a scan hides what is behind it, and what the view sees through is drawn over
it. A point's pixel holds its scan's slot after the triangles', so it picks, box-selects and tints as its element; it has no
face to paint and draws no edge. Points are not lit: they keep the scan's colours, grey (`cloud::GREY`) for a scan with none,
or the colour the view's projection fill gives. A picture draws at most `DRAWN` points, as Revit's density does, chosen by
level of detail (below). The view fits round its scans, and a level's plane reaches past them (`Model::model_extent`).
A 3D view's section box keeps a scan's points inside it and no others, so its floors and ceilings can be cut away to show the walls to
model against ([views and graphics](views-and-graphics.md)); in a plan the view range does it, or the box itself when the plan follows it. Views hide and override point clouds by element or by the category `pointcloud` (`graphics::SCANS`, apart from `MODEL`:
Revit has a tab of its own for them in Visibility/Graphics, and Object Styles no row). The graphics card draws them as the
processor does (below). The wireframe style leaves them out.

A section's or an elevation's picture is the same rasteriser with a `Clip`, and `Scene::dots` keeps a point as `raster` keeps a
pixel: between the line and the far clip, inside the crop. A plan draws what its view range sees, from the cut plane down to
the view depth (`render::scans`, under everything else), from above: each point a dot `DOT` pixels wide in its colour or the
view's, the highest at each pixel on top, `DRAWN` among the scans as in 3D, of the nodes whose cubes cross its range and its
edges. Each scan is one `Prim::Image` owned by its
element, no bigger than its dots and clear between them; a picture picks where it isn't clear (`drawn_near` in `Drawing::pick_all`),
so a click between a scan's points finds what is there, and a sheet's opaque pictures pick as they did. A PDF masks a picture
that isn't all there with its alpha (`/SMask`), and an SVG holds it as a PNG. A reflected ceiling plan leaves scans out. The
desktop's plan canvas loads a drawing's pictures as textures once for as long as it is shown (`PlanCache::pictures`) and, on a
dark canvas, dims them so the lightened ink over them still reads. A scan the view fills with one colour (its projection's fill,
`Look::filled`) is ink itself: its picture is marked `ink` and the dark canvas leaves it undimmed.

A scan's own colours are mid-tones that read poorly over paper or the dark canvas, so the view control bar (`footer.rs`) has
High Contrast Point Clouds wherever the project or a model it links holds a scan (the bar asks the model the views draw whether it holds
one, which is cheap because a command drops a scan nothing places): it fills the view's `pointcloud` category with `HIGH_CONTRAST`, a
crimson that reads on both and apart from the model's black lines (or the colour the user picks in Settings > Graphics, kept
as `high_contrast` in `settings.json` and read back only when it is `[r, g, b]`), through the `graphics` command (merged into the category's
override, so its halftone or lines stay), and again takes the fill away. It is lit whenever the category has a fill, whoever set
it, and greyed out when a view template controls the categories. It sets the category's override and no other: `Look::of` applies the
category's, then each view filter's, then the element's own, the last prevailing, so a scan given a colour of its own in
Visibility/Graphics (or picked out by a filter that fills) keeps it whatever the button does. The button is lit for the category
and can't tell, since what the scans come to is the whole `Look` for every frame of the bar; its tooltip says so instead.

A plan snaps to a scan where its cut plane cuts it, to trace what was measured: of the points within `snap::CUT_BAND` of the cut,
the nearest to the cursor seen from above (`Kind::Point`, "Point Cloud", switched by `Snaps::cloud` in Manage > Snaps). It comes
after what the model has to snap to (ends, middles, centres, crossings), so a wall's end beats a point nearer the cursor, and
before the perpendicular, the angle guides and a wall's centre line. `snap::cloud_point` turns the cursor into each shown scan's
own terms and asks `Cloud::nearest`, which looks only in the squares of the scan's `Grid` (its points by `CELL` mm squares from
above, made the first time something snaps to it and shared by its clones) that the reach touches, so a snap on millions of points
costs microseconds.

In the desktop (`clouds.rs`), Insert > Point Cloud, in the Insert tab's Link panel beside Link Model as in Revit, asks for one
file or several (`BimApp::ask_files`: a dialog's answer is a list of paths now) and opens `CloudBox`: its name (the first file's),
the units of its coordinates, a spacing, and Revit's positioning, Auto - Center to Center or Auto - Origin to Origin. OK runs
`point_cloud` on the level of the plan on screen and selects what it placed; a refusal stays in the box with its reason
(`point_cloud_paths` opens the box, what a test hands the files to). The Properties palette shows a point cloud's level and offset
(edited), its scan, file and points, its origin and angle; Visibility/Graphics lists `pointcloud` after the model's categories.

## Levels of detail

A scan is drawn through an octree of its points (`cloud/octree.rs`), as Potree made standard, built the first time a picture
draws it and shared by its clones (`Cloud::tree`), so a project file doesn't hold it. Its root's cube is the scan's box made
square; a node keeps one point in each square of a 64-square grid over its cube (first come first kept, the grid made coarser
should more than `NODE_MAX` squares be filled) and passes the rest to the eighth of its cube each lies in, whose grid is twice
as fine; a node given no more than `NODE_POINTS` keeps them all, as does one at `MAX_LEVEL`, so points all in one place end the
tree. The root is a coarse picture of the whole scan, not of where the file began, and each level down adds detail. The tree
holds the cloud's point indices grouped by node; the points stay as the cloud keeps them. The eighths are built on the threads
there are: twenty million points take under two seconds.

`octree::select` is what every picture draws: given how many pixels a millimetre of a node is drawn (none where it isn't seen),
it takes each scan's root, then the nodes drawn largest first while their points fit the budget, `DRAWN` among the scans, and
looks into a node only while its squares are drawn a pixel or wider (`FINEST`), since finer detail adds nothing. The 3D view and
sections leave out nodes past the picture's edges, outside the section box or a section's clip (`Scan::seen`); a plan, those
outside its view range or its edges. So a view zoomed in on part of a scan of many millions draws every point it sees, and one
of all of it draws its coarse levels. A project's scans hold up to `MAX_CLOUD_POINTS` (30 million) among them; past it, the file
is thinned as above. Snapping still looks through the `Grid`. How many points a picture draws is the user's: Settings >
Graphics' Points drawn (`point_budget` in `settings.json`, 0.1 to 20 million, `view3d::DRAWN_RANGE`), which the desktop gives
the core (`view3d::set_drawn`) for every picture after; a view's cached picture is drawn again when it changes.

## On the graphics card

`gpu::Mesh` holds the scans a 3D view shows, coloured as the view colours them (`graphics::Tone`), their elements among its
owners. `Mesh::chosen` is the processor's choice of nodes for a camera, and `Mesh::node_points` a node's points for the card,
from the mesh's origin. The desktop (`gpu.rs`) puts a node on the card the first time a picture chooses it, no more than
`UPLOAD` points a picture (a picture that is short of some says so, and the 3D view draws again the next frame), keeps up to
`KEPT` times the budget, and lets go first of the nodes drawn longest ago. In the opaque pass, after the surfaces, each point is
a square `DOT` pixels wide (`vs_point`, two triangles an instance), its corner rounded as the processor rounds it, unlit, with
depth; its pixels' plane is `POINT`, so `fs_post` draws no edge round it and tints it as the processor does. Picking through the
mesh finds the nearest point drawn over a pixel (`Mesh::nearest_point`) as the processor's picture would.

## Linked scans

A scan larger than a project holds is linked (`point_cloud`'s `link`, the default for a file over `LINK_ABOVE`, 256 MB; one file,
no `spacing`), as Revit links a ReCap project: every point of its file is kept in the user's cache (`cloud/cache.rs`), and the
project keeps only the upper levels of its tree, up to `PREVIEW_POINTS` (a million), to draw where the cache isn't. Its `Cloud`
holds a `Link`: the file's whole path, the millimetres in its unit, the fingerprint of the file (FNV-1a of its size, when it was
written, its first and last megabyte, how it's read and the cache's `VERSION`), and how many points it has. A project file holds
it as the cloud's `link` (format 10; `CloudFile`'s `points` are the preview). The cache is opened the first time something asks
(`Cloud::store`), found by the fingerprint in `cache::folder()`: `BIMCRAFT_SCAN_CACHE`, else the user's cache folder
(`%LOCALAPPDATA%\BIMCraft\scans`, `~/Library/Caches/BIMCraft/scans`, `$XDG_CACHE_HOME/bimcraft/scans`); tests and front ends may
say another (`cache::set_folder`). `Cloud::tree` is then the cache's, and `Cloud::node` a node's points, held (`NodePoints::Held`)
or loaded from the cache and kept a while (`NodePoints::Loaded`; a `Store` keeps `KEEP`, a gigabyte, and lets go first of the
blocks asked for longest ago), which is what the 3D view, plans, sections and the graphics card draw from; a load blocks, so a
picture is always whole. Snapping on a linked scan walks the nodes `reach` touches (`Cloud::nearest_in_tree`). `open` lists the
linked scans whose cache this machine hasn't (`clouds_not_loaded`), drawn by their preview until `reload_cloud` builds it again
from the file, as it does when the file has changed (it says `changed`).

The cache is built in bounded memory (`cache::build`): the file is read once to measure it, then once to sort its points into
the cubes of a grid of up to 16 along a side, each a file of its own beside the cache; a cube with more than `CHUNK_POINTS` (four
million) is split into its eighths, again and again, and one with fewer is built in memory as any node is (`octree::subtree`).
The nodes above are built from below, as Potree's converter builds them: each keeps one point in each square of its grid (made
coarser past `NODE_MAX`) of what its eighths' roots kept, and those leave the roots. A scan held in one chunk is the very tree a
held scan builds (`a_small_scans_cache_is_the_tree_a_project_builds_in_memory`). The file is a header, the nodes' blocks of points
(positions from the cloud's middle, then colours, then a CRC-32), and the table of nodes, root first, with its CRC in the
header, which has its own. A node no wider than 65,535 quarter millimetres (`STEP`, about 16 m: every node but the few at the
top of a large scan) packs its positions as 16-bit steps across its cube, half of `f32`'s bytes, and a quarter millimetre is
finer than a scanner measures; the others keep `f32`. The file is written beside its place and put there whole. A cache that's damaged, of another file or version,
or whose table isn't a tree (a child before its node, or with two) is refused, and built again; a damaged block is drawn as
nothing. A hundred million points build in about 70 seconds into 860 MB (`a_hundred_million_points_are_built_a_piece_at_a_time`,
ignored, from a file made as it's read), and a picture's two million of them load in under twenty milliseconds.

The desktop's Insert > Point Cloud box has Link the file (ticked for a large file), and builds the cache on a thread of its own
(`cloud::cached`), the box showing how far it has got on a gauge (`cache::Progress`: measuring the file, sorting its points,
building them) before `point_cloud` runs and finds it; Properties shows a linked scan's file,
and the right-click menu of a point cloud placing one has Reload Point Cloud, which builds on a thread too.
