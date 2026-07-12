export default function Modal({ title, children, footer, onClose }) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div
        className="absolute inset-0 bg-slate-950/70 backdrop-blur-sm"
        role="presentation"
        onClick={onClose}
      />
      <div
        role="dialog"
        aria-modal="true"
        className="relative flex w-[28rem] max-w-[90vw] flex-col rounded-3xl border border-slate-800 bg-slate-900/95 p-6 text-white shadow-[0_40px_90px_rgba(0,0,0,0.55)]"
        style={{ minHeight: "14rem", maxHeight: "80vh" }}
      >
        {title ? (
          <header className="flex items-start justify-between">
            <h2 className="text-lg font-semibold">{title}</h2>
            {onClose ? (
              <button
                type="button"
                onClick={onClose}
                className="rounded-full border border-slate-700/60 p-1 text-slate-300 transition hover:border-slate-500 hover:text-white"
                aria-label="Close dialog"
              >
                <svg
                  aria-hidden="true"
                  className="h-3 w-3"
                  viewBox="0 0 12 12"
                  fill="none"
                  xmlns="http://www.w3.org/2000/svg"
                >
                  <path
                    d="M3 3l6 6M9 3L3 9"
                    stroke="currentColor"
                    strokeWidth="1.5"
                    strokeLinecap="round"
                  />
                </svg>
              </button>
            ) : null}
          </header>
        ) : null}
        <div className="mt-4 flex-1 overflow-auto text-sm text-slate-200">
          {children}
        </div>
        {footer ? (
          <footer className="mt-4 flex justify-end gap-3">{footer}</footer>
        ) : null}
      </div>
    </div>
  );
}
