---
title: GET /api/ps
description: Union of running-model process lists — one row per loaded node.
sidebar:
  order: 6
---

`GET /api/ps` returns the **union** of every node's running-model list:
one row **per loaded node** (not per model name). A model loaded on three
nodes appears three times, each row identifying its node.

This mirrors the honest-fleet contract: `ps` is a process-list union, not a
single-daemon answer. No client activity is faked — this endpoint does not
count toward idle timers.
