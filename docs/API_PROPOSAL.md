# API

This is an work-in-progress and doesn't yet reflect the actual interface,
only what is planned.

## existing interface

### types

```
OutputName := String, alphanumeric, without any of "_-.";
BaseName   := String, without null bytes, and without any of "/\\";

StoreHash := [u8; 32] bytes, usually encoded in base64 => 43 chars encoded;

Regular := record {
    executable : bool,
    contents : array{u8},
};

ThinTree := varint {
    Regular(StoreHash),
    RegularInline(Regular),
    SymLink : record { target : String },
    Directory(map{BaseName, ThinTree}),
} untagged {
    Regular <=> "regular_hash",
    RegularInline <=> ["executable", "contents"],
    SymLink <=> "target",
    else: Directory,
};

WorkItem := record {
    args : array{String},
    envs : map{String, String},
    outputs : array{OutputName},
    files : map{OutputName, ThinTree},
};

TaskBoundResult := variant {
    Outputs(map{OutputName, StoreHash}),
    Errors : array{record {
        type : String,
        f : String,
        // if type.starts_with("store.")
        path : String,
    }},
};
```

### known error types

* `killed.by.client`
* `killed.by.signal.{signo}`
* `exit.{code}`
* `io.{errno}`
* `empty.command`
* `hash.collision:{...}`
* `unknown`
* `store.non_utf8.symlink_target`
* `store.non_utf8.basename`
* `store.unknown.file_type:{...}`
* `store.invalid.basename`
* `store.unsupported.symlinks`
* `store.overwrite.declined`
* `store.io.{errno}`

### functions

```
GetStorePath: () -> String
    retrieves the server's store path,
    necessary to reconstruct store paths from hashes.

(Submit)Task: WorkItem -> Logs ... TaskBoundResponse/Result
    schedules a task, attaches to the logs...
    the last line contains the result, which may be an error,
    or a map of outputs.

HasOutHash: StoreHash -> bool
    checks if a given hash is present in the store

Upload: ThinTree -> bool
    uploads a given serialized tree into the store

Download: StoreHash -> ThinTree!Error
    downloads a tree selected by hash, may return an error

```

(HasOutHash, upload and download should also be present for regulars...)

## new interface

* JSON, types stay roughly the same

### API

```
/info
  GET
    when requested as application/json:
    """
    {
        "StoreDir": "/yzixs",
        "LogCompression": "zst",
    }
    """
    when requested as text/x-nix-cache-info:
    """
    StoreDir: /yzixs
    LogCompression: zst
    """

/task
  POST
    request format: WorkItem as JSON.
    response: log messages as simple strings prefixed with ".",
      last line contains the result (JSON, either errors, or map of outputs).

/{...}
  just serves the store directory directly

  HEAD
    presence check
  GET
    download

  {...}=
    .links/*  ... regular files
    *.log.*   ... log files
    *         ... store dirs

/store/{hash}/thintree
  HEAD
     presence check, e.g. HasOutHash
  GET
     download
  PUT
     upload (hash gets verified, and the upload will get rejected if the hash mismatches)

/regulars/{hash}
  same as `/thintrees/{hash}`, but for regular files.
  the executable bit will be transmitted using an `X-Executable: [true|false]` header.
```

Authorization happens via `Bearer` tokens.
Authorization is only required for endpoints which modify data, e.g. `POST`, and `PUT` requests.
