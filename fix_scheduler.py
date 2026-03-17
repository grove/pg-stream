with open("src/scheduler.rs", "r") as f:
    text = f.read()

import re

# We need to find the place where execute_worker_atomic_group executes its transaction.
# Let's search for BEGIN / COMMIT in scheduler.rs.
