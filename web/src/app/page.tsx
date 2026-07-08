import { Suspense } from "react";

import { SearchApp } from "./search-app";

export default function Home() {
  return (
    <Suspense>
      <SearchApp />
    </Suspense>
  );
}
