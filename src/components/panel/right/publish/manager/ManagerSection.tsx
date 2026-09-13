import { useEffect, useRef, useState } from 'react';
import clsx from 'clsx';

import CollapsibleSection from '../../../../ui/CollapsibleSection';

/** `CollapsibleSection`'s height transition. */
const TRANSITION_MS = 300;

interface ManagerSectionProps {
  title: string;
  /** Scrolls the section into view, opening it if need be. */
  isRequested?: boolean;
  children: React.ReactNode;
}

/**
 * A `CollapsibleSection`, open to start with, whose content may overflow once
 * it has finished opening. The section clips its content so it can animate,
 * which would otherwise cut off a `Dropdown`'s list.
 */
export default function ManagerSection({ title, isRequested = false, children }: ManagerSectionProps) {
  const [isOpen, setIsOpen] = useState(true);
  const [canOverflow, setCanOverflow] = useState(true);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isOpen) {
      setCanOverflow(false);
      return;
    }
    const timer = setTimeout(() => setCanOverflow(true), TRANSITION_MS);
    return () => clearTimeout(timer);
  }, [isOpen]);

  useEffect(() => {
    if (!isRequested) return;
    setIsOpen(true);
    ref.current?.scrollIntoView({ block: 'start' });
  }, [isRequested]);

  return (
    <div
      ref={ref}
      className={clsx(
        canOverflow &&
          '[&>div]:overflow-visible [&>div>div:first-child]:rounded-t-lg [&>div>div:last-child]:overflow-visible',
      )}
    >
      <CollapsibleSection
        canToggleVisibility={false}
        isContentVisible={true}
        isOpen={isOpen}
        onToggle={() => setIsOpen((open) => !open)}
        title={title}
      >
        <div className="space-y-3">{children}</div>
      </CollapsibleSection>
    </div>
  );
}
