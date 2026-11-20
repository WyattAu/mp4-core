# Pattern adoption

| Pattern | Status | Evidence |
|---|---|---|
| T1 | adopted | src/container.rs:310 (parse_boxes turns bytes into a typed box tree or an Mp4Error; size encodings checked at parse, src/container.rs:326) |
| T13 | adopted | src/container.rs:310 (pure box-tree model and serialization), src/io.rs:1 (seek-based random-access reader is the I/O shell) |
