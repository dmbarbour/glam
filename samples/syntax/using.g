language g0

import 'std

outer = "outer"
namespace = {
  value:"inner",
  emit:(\value -> .r value)
}

selected = using namespace in value
escaped = using namespace in ^outer
selfSelected = using namespace in self.value

effectful = list.pure (using namespace do
  emit selected)

explicitDo = list.pure (using namespace in do
  emit escaped)
