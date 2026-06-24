"use client";

import React from "react";
import {
  DndContext, closestCenter, PointerSensor, KeyboardSensor,
  useSensor, useSensors, type DragEndEvent,
} from "@dnd-kit/core";
import {
  SortableContext, sortableKeyboardCoordinates, useSortable,
  verticalListSortingStrategy, arrayMove,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import type { Provider } from "@/types/api";
import type { ProviderCategory } from "./_parts/constants";
import { ProviderRow } from "./ProviderRow";

interface ProviderSortableGroupProps {
  cap: ProviderCategory;
  activeProviders: Provider[];
  typeLabelFor: (p: Provider) => string;
  onReorder: (orderedNames: string[]) => void;
  onToggleActive: (p: Provider) => void;
  onEdit: (p: Provider) => void;
  onDelete: (p: Provider) => void;
}

function SortableRow({
  provider, cap, typeLabel, onToggleActive, onEdit, onDelete,
}: {
  provider: Provider;
  cap: ProviderCategory;
  typeLabel: string;
  onToggleActive: () => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } =
    useSortable({ id: provider.name });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    zIndex: isDragging ? 10 : undefined,
  };
  return (
    <ProviderRow
      ref={setNodeRef}
      style={style}
      provider={provider}
      cap={cap}
      isActive
      typeLabel={typeLabel}
      isCapabilityGroup
      draggable
      isDragging={isDragging}
      dragHandleAttributes={attributes}
      dragHandleListeners={listeners}
      onToggleActive={onToggleActive}
      onEdit={onEdit}
      onDelete={onDelete}
    />
  );
}

export function ProviderSortableGroup({
  cap, activeProviders, typeLabelFor, onReorder, onToggleActive, onEdit, onDelete,
}: ProviderSortableGroupProps) {
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const names = activeProviders.map((p) => p.name);
    const oldIndex = names.indexOf(String(active.id));
    const newIndex = names.indexOf(String(over.id));
    if (oldIndex < 0 || newIndex < 0) return;
    onReorder(arrayMove(names, oldIndex, newIndex));
  };

  return (
    <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={handleDragEnd}>
      <SortableContext items={activeProviders.map((p) => p.name)} strategy={verticalListSortingStrategy}>
        <div className="space-y-2">
          {activeProviders.map((p) => (
            <SortableRow
              key={p.id}
              provider={p}
              cap={cap}
              typeLabel={typeLabelFor(p)}
              onToggleActive={() => onToggleActive(p)}
              onEdit={() => onEdit(p)}
              onDelete={() => onDelete(p)}
            />
          ))}
        </div>
      </SortableContext>
    </DndContext>
  );
}
