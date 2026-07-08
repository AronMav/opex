"use client";

import { forwardRef } from "react";

// Virtuoso injects its own scroller/list/item wrapper <div>s, so stamping
// role="listitem" onto the itemContent output leaves the items orphaned from any
// role="list" ancestor (the intervening generic divs sever the ARIA
// list→listitem ownership). Overriding Virtuoso's List and Item components makes
// the listitem a DIRECT child of the list, which is what screen readers require
// for "list, N items" / positional announcements.

// Virtuoso only injects a `context` prop when <Virtuoso context={…}> is set,
// which these lists don't use — so spreading props straight onto the div is safe
// and carries only style / data-* attributes Virtuoso needs for measurement.
type VirtuosoSlotProps = React.HTMLAttributes<HTMLDivElement>;

export const VirtuosoList = forwardRef<HTMLDivElement, VirtuosoSlotProps>(
  function VirtuosoList(props, ref) {
    return <div {...props} ref={ref} role="list" />;
  },
);

export const VirtuosoListItem = forwardRef<HTMLDivElement, VirtuosoSlotProps>(
  function VirtuosoListItem(props, ref) {
    return <div {...props} ref={ref} role="listitem" />;
  },
);
