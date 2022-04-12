# yzix-proto

## Architecture

The server is simpler than the one in `yzix-v0`, because it doesn't work on a
graph. Instead, it works on fully resolved `WorkItem::Run`' items.

```rust
#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub enum WorkItem {
    /// node name is intentionally not present here,
    /// as it is irrelevant for execution and hashing
    Run {
        args: Vec<String>,
        envs: HashMap<String, String>,
        /// invariant: `!outputs.is_empty()`
        outputs: HashSet<OutputName>,
        // obsolete:
        //new_root: Option<StoreHash>,
        //uses_placeholders: bool,
        // new:
        autosubscribe: bool,
    },
    // [...]
}
```

To the clients, the server not only allows task submission, but also
acts as a subscription server for log output. A client can send subscribe
requests for inhashes.

## Interface

Thus, available requests:
- `logsub({taskid, bool})`: subscribe(true)/unsubscribe(false) to log output
- `submitTask({args, envs, new_root, outputs, autosubscribe })`
- `upload(dump)`: serialize a dump back into the store
- `hasOuthash(outhash)`: check if the outhash is present in the store
- `download(outhash)`: retrieve a dump of the object identified by outhash from the store

Possible responses:
- `ok`
- `false`
- `error(...)`
  - `overflow` (e.g. no more task id's available)
- `log({taskid, line})` (out of band, unrelated to concrete requests)
- `dump(dump)`
- `taskid(taskid)` (ret for `submitTask`)
