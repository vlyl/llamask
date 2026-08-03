import type { SVGProps } from "react";

type IconProps = SVGProps<SVGSVGElement>;

const defaults = {
  width: 20,
  height: 20,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.8,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
};

export function ShieldIcon(props: IconProps) {
  return (
    <svg {...defaults} {...props}>
      <path d="M12 3 4.8 6v5.2c0 4.7 2.9 8.2 7.2 9.8 4.3-1.6 7.2-5.1 7.2-9.8V6L12 3Z" />
      <path d="m8.7 12 2.1 2.1 4.7-4.7" />
    </svg>
  );
}

export function PlusIcon(props: IconProps) {
  return (
    <svg {...defaults} {...props}>
      <path d="M12 5v14M5 12h14" />
    </svg>
  );
}

export function FileIcon(props: IconProps) {
  return (
    <svg {...defaults} {...props}>
      <path d="M6.5 3.5h7L18.5 8v12.5h-12z" />
      <path d="M13.5 3.5V8h5" />
    </svg>
  );
}

export function ClipboardIcon(props: IconProps) {
  return (
    <svg {...defaults} {...props}>
      <rect x="5" y="5.5" width="14" height="15" rx="2" />
      <path d="M9 5.5V4.8A1.8 1.8 0 0 1 10.8 3h2.4A1.8 1.8 0 0 1 15 4.8v.7M8.5 10h7M8.5 14h7" />
    </svg>
  );
}

export function FolderIcon(props: IconProps) {
  return (
    <svg {...defaults} {...props}>
      <path d="M3.5 6.5h6l2 2h9v10h-17z" />
    </svg>
  );
}

export function SettingsIcon(props: IconProps) {
  return (
    <svg {...defaults} {...props}>
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.8 1.8 0 0 0 .3 2l.1.1-2.7 2.7-.1-.1a1.8 1.8 0 0 0-2-.3 1.8 1.8 0 0 0-1.1 1.6v.2h-3.8V21A1.8 1.8 0 0 0 9 19.4a1.8 1.8 0 0 0-2 .3l-.1.1-2.7-2.7.1-.1a1.8 1.8 0 0 0 .3-2A1.8 1.8 0 0 0 3 13.9h-.2v-3.8H3A1.8 1.8 0 0 0 4.6 9a1.8 1.8 0 0 0-.3-2l-.1-.1 2.7-2.7.1.1a1.8 1.8 0 0 0 2 .3A1.8 1.8 0 0 0 10.1 3v-.2h3.8V3A1.8 1.8 0 0 0 15 4.6a1.8 1.8 0 0 0 2-.3l.1-.1 2.7 2.7-.1.1a1.8 1.8 0 0 0-.3 2 1.8 1.8 0 0 0 1.6 1.1h.2v3.8H21a1.8 1.8 0 0 0-1.6 1.1Z" />
    </svg>
  );
}

export function TrashIcon(props: IconProps) {
  return (
    <svg {...defaults} {...props}>
      <path d="M4.5 7h15M9 7V4.5h6V7m2.5 0-.8 13h-9.4L6.5 7M10 10.5v6M14 10.5v6" />
    </svg>
  );
}

export function ChevronRightIcon(props: IconProps) {
  return (
    <svg {...defaults} {...props}>
      <path d="m9 5 7 7-7 7" />
    </svg>
  );
}

export function LockIcon(props: IconProps) {
  return (
    <svg {...defaults} {...props}>
      <rect x="5" y="10" width="14" height="10" rx="2" />
      <path d="M8.5 10V7.5a3.5 3.5 0 0 1 7 0V10" />
    </svg>
  );
}

export function CloseIcon(props: IconProps) {
  return (
    <svg {...defaults} {...props}>
      <path d="m6 6 12 12M18 6 6 18" />
    </svg>
  );
}
