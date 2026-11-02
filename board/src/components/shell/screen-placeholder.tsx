// The screens after the shell (T3.2 to T3.5) replace their route files; until
// then each route renders its name and where it will come from.
export function ScreenPlaceholder({ title, source }: { title: string; source: string }) {
  return (
    <section className="placeholder">
      <h2>{title}</h2>
      <p>Built in {source}.</p>
    </section>
  );
}
