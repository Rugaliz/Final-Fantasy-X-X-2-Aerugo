# The script machine

Scripts (see [formats/script.md](formats/script.md)) run on a small stack machine, implemented in `aerugo-script`.

## Workers and levels

Every worker of a script is an entity with its own operand stack (19 slots tagged int or float), 4 integer and 10
float registers, and one execution context per **priority level** (0–8; two levels for some worker kinds). A context
holds the program counter, a three-deep call stack for calls into other workers' code, the A, X and F registers, and
what it is waiting for. Workers of kind 0 hold subroutines only and never run on their own.

Each frame a worker runs instructions at its current level until it yields **and** its stack is empty. It yields
when a native call has not finished, when it issues a waiting request, when it ends an entry, or when it waits for
another worker. A runaway worker is stopped after 65,536 instructions in one frame.

## Requests

Workers ask each other (and the engine asks workers) to run an entry at a level. Requests queue on the target,
highest level first; a target refuses a second request at a level it already has one for. After all workers ran,
each worker's queue is checked: a request above the current level starts (or a preempted one resumes). Requesters
can wait until the entry starts or until it ends; the target answers with a signal. Ending an entry takes the next
request, and can cancel requests above a level. An idle worker sits on its end instruction and runs it again every
frame, which is how it picks up new work.

## Events

An event first runs every worker's entry 0 until each has ended once (the init phase), then runs all workers every
frame.

## Natives

Natives are engine functions called by id. A call has up to three steps: start (take arguments), poll once per frame
until done, and produce a result. `Natives<G>` registers implementations over a game state `G`. A native Aerugo does
not implement yet takes the arguments it would take (from a table of argument counts), returns zero and is logged
once; `cargo run -p xtask -- native-coverage` runs every script and lists the most used missing natives.

## Memory

Variables address two global blocks (the first is the save image), the worker's data areas in the script, the
script's shared data and the worker's registers. Values are converted, truncated or saturated as the variable's
type requires. `push.addr` hands natives an encoded address of a variable element.

## Checking against the original

Every executed instruction can be traced (frame, worker, level, pc, opcode) and compared with recordings of the
original game (below); without recordings the machine is checked with
assembled test scripts and by running every script of both games.

`aerugo-script::recording` reads recordings of the original game (instructions per frame, the scripts' random
numbers, event starts, map changes). `cargo run -p xtask -- oracle-compare <folder>` replays each recorded event in
Aerugo from the recorded save image with the recorded random numbers, and reports the first instruction where the
two differ. Player input is not replayed yet, so comparisons hold until the player acts.
