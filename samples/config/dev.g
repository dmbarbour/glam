language g0

# Default workspace configuration for the devcontainer.
#
# Keep this boring. It should make local development convenient without hiding
# behavior that future tests should set explicitly.
import "minimal.g"

conf.env := _conf.env with
  sample_root = "samples"
