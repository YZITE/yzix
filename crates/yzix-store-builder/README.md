# yzix-store-builder

## internal architecture

### logging

Clients can subscribe to the output of work items.
A single async task on the server collects all log outputs and redistributes them.
