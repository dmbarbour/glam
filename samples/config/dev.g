language g0

# Default workspace configuration for the devcontainer.
#
# This should (eventually) provide common utility functions for assembly (via 'conf.env').
# But most other configuration options should be separated for testing.
import "minimal.g"

extend conf.env with
  hello_message = "Hello from conf.env!"
