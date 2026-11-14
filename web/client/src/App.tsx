import { MotionConfig } from 'framer-motion';
import { Route, Routes } from 'react-router';

import { Footer } from './components/Footer';
import { GridBackground } from './components/GridBackground';
import { Header } from './components/Header';
import { ScrollManager } from './components/ScrollManager';
import { AppShell } from './pages/AppShell';
import { Docs } from './docs/Docs';
import { Landing } from './pages/Landing';
import { NotFound } from './pages/NotFound';

export function App() {
  return (
    <MotionConfig reducedMotion="user">
      <GridBackground />
      <ScrollManager />
      <a className="skip" href="#content">
        Skip to content
      </a>
      <div className="shell">
        <Header />
        <main id="content" className="content" tabIndex={-1}>
          <Routes>
            <Route path="/" element={<Landing />} />
            <Route path="/app" element={<AppShell />} />
            <Route path="/docs" element={<Docs />} />
            <Route path="*" element={<NotFound />} />
          </Routes>
        </main>
        <Footer />
      </div>
    </MotionConfig>
  );
}
